# Plan — resolve GitHub issues #26–#34 (branch `fix/github-issues-26-34`)

All nine issues were raised by @app/omp-harness (enhancements). Each was
verified against the actual code before acceptance; findings that failed
verification are called out below. Judgment calls pending user ruling live in the
"Open decisions" section; rulings already made are recorded there as decided.

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

1. Branch off main (`9219dc5`): `fix/github-issues-26-34`. Repo clean.
2. `make self-check` + `pytest` green; `make scanner-check` builds.

## Phase 1 — Catalog (`rule_metadata.py` → `make rules`)

New families (severities per Open decisions 1 and 5):

| Issue | Kind | display_group | Fix kind |
|---|---|---|---|
| #26 | `duplicate-module` | `None` (standalone — its own name is the group) | none (reconciliation is judgment) |
| #27 | `static-husk` | `None` | none |
| #27 | `delegating-husk` | `None` | dissolve-husk (rewire callers) |
| #28 | `process-class` (supersedes `single-use-class` — dropped 2026-09-30; the dead-class arm folds into `unused`) | `None` | none (apply the domain test: fold/rename/delete) |
| #30 | `forwarding-chain` | `None` | collapse-chain (multi-file, identity-gated) |
| #31 | extend `class-module` (no new kind) | existing (`"standard"`) | split-module (new multi-file fixer) |
| #32 | `closure-cluster` | `"latent-class"` (FAMILY_VARIANTS is DERIVED, not joined) | none (extraction refused by design) |

Naming (2026-09-29): the bucket field is `display_group` — `None` means the rule
belongs to no display group and stands alone under its own kind; `"standard"` is the
catch-all group; any other value is a family group (`latent-class`, `docs`,
`loop-pipeline`). The RULES.md-section field yields the name: `Rule.display_group`
(section: architecture/style/...) becomes `Rule.section`; `Rule.display` (bucket)
becomes `Rule.display_group`. Field-rename touches: the catalog rows' kwarg names,
`gen-rules.py` (bucket = display_group or kind; group indexing by section), the
`_CONFIG_GROUP` map, RULES.md static text, and the generated artifacts via `make
rules`.

Also register `Rule.judgement`-free stamp: judgement is DERIVED from the existing
`fix_name_required` flag (ruling 2026-09-29, Open decisions 5 — no new metadata), and
fix the stale `partition` description in the same catalog edit.

## Phase 2 — Scanner (Rust)

- **#26 duplicate-module** — extend the Dice machinery at checks.rs:5560
  (`BigramInterner`, `dice_from_bigrams`) with module/class skeletons: `NAME = literal`
  constants (capitalised, literal normalised by type) + statement bigrams minus
  imports/docstrings. Pair identity: module-module, class-class, module-class. Threshold
  0.9 hardcoded — configurable thresholds are NOT supported (ruling 2026-09-29, see
  Open decisions 3). Exclusions: `__init__.py`, <3 statements,
  vendored dirs, tests. `ignore-file duplicate-module` opt-out works per-kind.
- **#27 static-husk** — zero instance state (no `self.X =` anywhere, no dataclass fields,
  no properties) AND all members staticmethods, >=1 method. Exclusions: inherited base,
  ABC/Protocol, `pass`-only subclass, `*Error`/`*Exception`. warn.
- **#27 delegating-husk** — every instance method's body is exactly one `return`
  expression calling a module-level function passing all params through. warn; depth >=2
  moves to #30. Facade opt-out is a plain `lucidlint: ignore delegating-husk <why>`.
  Fix: dissolve — rewire the husk's callers to the module functions, delete the husk
  (it has no identity by definition). Never inline the functions' bodies into it.
- **#28 process-class — single-callsite DROPPED (ruling 2026-09-30)**. Construction
  count is irrelevant to lucidity; the de-baseline junk is caught by process-naming +
  hollowness. New rule: a class whose name is process-shaped (-er/-or verb-noun set:
  Builder, Importer, Exporter, Validator, Converter, Parser, Reader, Writer, Fetcher,
  Collector, Handler, Manager, Provider, Renderer, Serializer, Dispatcher, Processor,
  Analyzer, Scheduler, Runner, Executor, Authenticator, ... — enumerated; a general
  `-er` test would fire on real nouns) — EXCLUDING GoF pattern names (Visitor,
  Iterator, Command, Factory, Strategy, Proxy, Adapter — the domain's own lexicon).
  fail (naming defect, `vague-name` precedent). Message carries the four-part honesty
  test (see Fourth-pass 4). The zero-site (never constructed) arm becomes: extend
  `unused` to CLASSES (warn, Python — "a class never referenced anywhere" via the
  existing reference machinery; test-used is not dead).
- **#31 class-module multi** — remove the `classes.len() != 1` returns (also the
  DUPLICATED `__init__.py`/len block at checks.rs:2687-2693 — a merge artifact, clean
  it while here); add condition: module (non-test, not `__init__`) with >=2 public
  classes where AT LEAST ONE public class's name doesn't match the stem
  (case-insensitive, plural-underscore form ok). A matching class EXCUSES NOTHING
  (ruling 2026-09-30): the finding names the MISPLACED classes — a class belongs in
  a file named after it, regardless of whether a sibling already matches the stem.
  Keep tool-script/test/exceptions. fail (matches existing). Fix directive:
  `— fix: split-module`. Fix operates on the misplaced classes only: the stem-named
  class (if any) STAYS in the origin module. Fixture migration:
  `class_module_matching_name_and_multi_class_pass` (main.rs:3485, User/Team, neither
  matches) is INVERTED into the new arm's finding test; add a PASS fixture where
  EVERY class matches the stem (PlanCritique BQ6 — the plan's original evidence
  fixture contradicts its own rule).
- **#32 closure-cluster** — local-var x nested-fn bipartite graph per method; >=2
  connected components each >=2 nested fns and >=1 distinct local ⇒ cluster. Degenerate
  arm: empty-param method >=150 lines with nested defs. warn (severity ruling
  2026-09-29 — message can be wrong, extraction can hurt). Co-reporting with
  `closures`/`large-function` is ACCEPTED (dedupe across families is out of scope).
  Refusal citation: the current extract-method refusal for nested targets returns
  `(None, None)` with NO decline text (fix_engine.py:260-261) — implementing "the
  refusal cites closure-cluster" means ADDING decline-text plumbing on the Python
  side, using the >=2-nested-def proxy: the bipartite cluster graph lives in the
  scanner; the fix engine sees one file and must not reimplement the detection.
- **#30 forwarding-chain detection** — structural same-repo resolution: method M body
  exactly `return F(<all M params>)` (or F as the sole expression), module fn F body
  exactly `return N(<all F params>)` where N is a method on ANOTHER class D; depth >=2
  edges. No graph tool required. fail (severity ruling 2026-09-29) WITH the
  DEPENDENCY-BOUNDARY GATE (2026-09-30): the chain is only dead indirection when the
  modules are ALREADY coupled — C's module imports/references D's module, or C and D
  are in the same module. When C's module and D's module are otherwise independent,
  the chain is the ONLY coupling path — a deliberate boundary (the issue's own
  other's classes) — NO finding, NO message. (No finding means no message: the
  flagged finding fires only in the coupled case, where the boundary rationale does
  not apply and no caveat belongs in its message.)
- **#34 structured seam** — new finding field carrying clump members: data-clump's
  function names + parameter pairs, partition's groups, strewing's names. NO message
  parsing (agreed seam source). NO compatibility seam: the scan contract bumps to
  schema_version 4 (ruling 2026-09-29 — backwards compatibility is NOT supported); the
  field is part of the new contract, and the orchestrator's existing strict version
  check (lucidlint.py:825) rejects old-schema output loudly — never silently
  defaulted. Bump touches: the version check constant, its pinning test, and any
  schema doc.

## Phase 3 — Fix engine (Python, libcst; fix.rs for Rust where shape applies)

- **#30 collapse-chain — IN SCOPE (ruling 2026-09-29: full multi-file fix)**. The
  transform is identity-gated and NEVER blindly inlines (user requirement 2026-09-29).
  Chain C.M -> F -> D.N with M, F pure forwarders, F single-caller:
  - **DEPENDENCY GATE** (2026-09-30) — refuse when C's module does not already depend
    on D's module: collapsing would marry two independent modules (the chain is the
    deliberate boundary).
  - **IDENTITY GATE** — D has data members (self.X assigns, declared/dataclass fields)
    or any non-forwarding member -> D is real: **rewire, not inline** — M calls
    D.N(<args>) directly, F deleted. Inlining would duplicate D's behavior into C and
    erase D's role.
  - D has NO identity (no state, no behavior beyond forwarded calls) -> N is a method
    in exile: **promote N to a sibling method of M on C** (`self.N(...)`); M's body
    becomes the call (or M merges into N); F deleted; D dissolves. INLINE N's body into
    M only as the fallback when the move is refused (N has other callers whose rewrite
    to a C instance is unsafe, or a name collision on C) — inlining preserves N for
    its other callers.
  - REFUSALS (all shapes): >1 caller of F; `*args`/`**kwargs` passthrough; callee N in
    third-party code (no source); keyword-name collisions after substitution; any
    `from mod import F` re-export reference (deleting F breaks the importer).
  - Multi-file transaction: repo-wide single-caller determination, cross-file call-site
    rewrite, cross-file member move, delete F, per-file py_compile + re-scan showing
    the chain gone. Scope per Open decisions: full fix, this round.
- **#31 split-module — IN SCOPE (ruling 2026-09-29)**. New structural fixer (libcst).
  Two layouts; the choice is a CORRECTNESS decision, never a safety one (the transform
  is deterministic and safe for both; user requirement 2026-09-29).
  - **PACKAGE layout** — convert `mod.py` -> `mod/` package: `<class>.py` per public
    class (statically-computed import set) and `__init__.py` re-exporting every class
    (`from .row import Row`); `from mod import Row` / `mod.Row` keep resolving with
    ZERO caller rewrites. (No constants in `__init__.py` — third-pass 5: they move
    into the owning class.)
  - **FLAT layout** — the issue's original spec: `<class>.py` siblings next to the
    module; rewrite `from old import Cls` -> `from new import Cls` and `old.Cls` ->
    `new.Cls` repo-wide to a fixed point.
  DECISION RULE (correctness — cohesion ONLY, no container-word test, 2026-09-29):
  the split's SUBJECT is the public classes whose names don't match the stem (the
  misplaced classes; a stem-matching class, if any, STAYS in the origin — it is
  home, and excuses nothing for the others). Cluster the subject by edges =
  cross-class member references OR shared class-level attributes (module-level
  constants do not exist as a signal — third-pass 3 forbids them). The stem's part
  of speech is irrelevant; what matters is whether the CLASSES belong together.
  Per cluster of the subject:
  - all misplaced classes form ONE cluster -> **PACKAGE**; named by the stem when no
    class matches it (the stem is free), else via `--name <package>` (the stem
    belongs to the staying class);
  - a cohesive sub-cluster (>=2 misplaced classes, edges among them) -> **PACKAGE**
    via `--name <package>` (never flat-for-convenience; the name is the commitment);
  - a singleton misplaced class (no edges) -> **FLAT** file named after it.
  (The existing "closely related models" carve-out in the message stays.)
  REFUSE (refusals are transform-safety only — the fixer declines with a reason and
  the agent applies by hand): an existing `mod/` directory when flat is also
  impossible; module-level executable statements BETWEEN classes (they run in
  sequence; splitting would reorder them); an import cycle in the candidate layout; a
  target file collision in both layouts. SAFETY: per-file py_compile + the rule's own
  re-scan showing the stems now match. Refusal/preview surface follows the existing
  extract-module fixer (`fix_engine.py`) — same refusal pattern, class-level targets.

## Phase 4 — Orchestrator (lucidlint.py; Action model)

- **#33(a) stamp** — `judgement: bool` field on Action → `--json` actions; TEXT kind
  header gains `JUDGEMENT` for judged findings and `MECHANICAL` for the deterministic
  rewrite kinds (issue #33(a) requires both stamps; findings without a fix directive
  get neither). Judgement set — DECIDED (2026-09-29): **anything where the user must
  provide a name**. Definition: the finding's fix kind (via `fix_kind_of`,
  gen-rules.py:203-216) is in NAME_REQUIRED_KINDS (rules_gen.rs:134) — i.e. complexity,
  feature-envy, long-param-list, loop-hoist, magic-number, tuple-record, vague-name,
  record-shape (extract-record-class), module-cohesion (extract-module), and the
  extract-class latent-class variants (data-clump, partition, strewing, wide-tuple).
  Supersedes the earlier derived-ten: feature-envy, magic-number and loop-hoist JOIN
  (they are name-required; the "name-required but mechanical" carve-out is gone).
  NOTE: data-clump and partition currently carry NO fix directive (PlanCritique
  C-notes) — they gain `— fix: extract-class` in the scanner message so the stamp
  applies. Mechanical := fixable AND fix kind NOT name-required (loop-pipeline /
  loop-sequence, positional-literals, stale-suppression, noop-statement, unreachable,
  duplicate-def, restating-docstring, duplicate-block, undeclared-attribute).
  No new metadata: judgement is DERIVED from the existing catalog `fix_name_required`
  (already driving NAME_REQUIRED_KINDS, LSP needsName, CLI refusal) — one source of
  truth, the stamp cannot drift from the fix surface.
- **#33(b) NAMING notice — ONCE PER REPORT, not per finding (ruling 2026-09-29)**.
  When >=1 judgement-stamped finding exists, print the notice once at the top of the
  findings list (after the header); judgement findings reference it via their
  `JUDGEMENT` stamp — no preamble copied onto messages. JSON: `meta.naming_notice`
  (present iff any judgement finding) + per-action `judgement: true`. Notice text
  (draft, 2026-09-29 — research-sourced: DDD stateless-services rule, the no-Manager
  canon, McConnell):
  "NAMING — the name is the commitment: it must be what the domain calls the THING,
  not what the code does to it. (1) Read this module's and the domain's existing nouns
  first — if a type already carries this group, extend it, never mint a twin. (2) A
  verb name (-er/-or: Builder, Importer, Validator, Capture) names a process: if it
  holds state, that state IS the thing — name the class by it (the session, the
  queue, the checkout); if it holds none, it's functions, not a class. (3) An -er/-or
  name is honest only when the domain itself calls a stateful component that (the
  parser, the compiler, the scheduler) and no existing type already is it. (4) Names
  that describe the means, not the thing: generic containers (Options, Context,
  Parameters, Config) and this tool's own jargon (Seam, Clump, Accumulator) — the
  thing has a domain noun." Scanner messages stay clean; LSP unchanged this round
  (direct-to-binary, documented gap).
- **#33(c) name-suffix nudge — HELD (ruling 2026-09-29, option A)**. No post-hoc
  note; naming is steered first time by the notice + stamp. The recorded decision D
  (no second-guessing names) governs.
- **#33(d) existence check — folded into the notice's point 1**. No per-message copy.
- **#34 clustering** — report layer: >=3 findings sharing a seam group under
  "these N findings share ONE seam — design the target type once, for all of them (the
  seams: ...)". Seam = (file, function) for extract findings; member-set intersection via
  the new structured field for latent-class findings. JSON: non-breaking `groups` array
  beside flat `actions`. Deterministic ordering.

## Phase 5 — Verification

- Per rule: scanner unit test (fixture in `tests/fixtures/rust/` — the include_str!
  convention; NOT `scanner/tests/fixtures/`, which does not exist), suppression test
  (`lucidlint: ignore` + config `ignore`), orchestrator test where gate behavior
  changes.
- `cargo test` green; `make rules --check` (drift); `make check` (ruff + pyrefly);
  `pytest` green.
- `make self-check` — new families on lucidlint's own repo: expect hits
  (e.g. process-named classes in lucidlint.py/fix_engine.py; closure-cluster/static-husk
  on fix_engine.py). Hygiene for hits: suppress each with a `lucidlint: ignore <signal>
  <why>` comment at the site — baselines CANNOT carry whys (bare kind:file:function
  keys, lucidlint.py:1661) and new WARN findings never gate; the round still ends with
  no unsuppressed hit (that is the house standard, not the gate).
- `make coverage` refresh; commit; PR to main (house pattern:
  previous round was `fix/github-issues-21-22-23` → PR #24).

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
   name.** Judgement := the finding's fix kind is in NAME_REQUIRED_KINDS (derived from
   the existing catalog `fix_name_required` flag — no new metadata). Includes
   feature-envy, magic-number, loop-hoist (name-required — the earlier
   "name-required but mechanical" carve-out is gone); mechanical := fixable without a
   user-supplied name; unstamped := no fix directive. See Phase 4 #33(a).
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
   carries the four-part honesty test. No `fix_name_required` (fold/rename/delete is
   judgment), so unstamped.
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

1. **Judgement = the catalog `fix_name_required` flag, period.** One read, no
   derivation tables. The earlier fix_kind_of/message-directive machinery is DROPPED —
   it duplicated knowledge the flag already owns. Set the flag on the rows that
   semantically require a name but lack it: `large-function` (real existing drift —
   its extract-method fix demands a name yet the flag is absent, so derived
   NAME_REQUIRED_KINDS + LSP needsName miss it), `partition`, `strewing`,
   `wide-tuple`. Message directives NOT changed this round; the flag-without-directive
   state is accepted (data-clump precedent, verified: flag set, message prose-only).
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
   domain objects it operates on, or in functions. A process class is the best
   idea only when ALL hold — (1) the domain itself names it (the parser, the
   scheduler); (2) its state is its own and substantial, no actual noun can
   carry it; (3) no existing type already carries this work (extend or fold,
   never mint a twin); (4) it is not one owner's private device (a single
   consumer with no own state is a function, not a class). Mostly one fails:
   fold, rename, or delete."
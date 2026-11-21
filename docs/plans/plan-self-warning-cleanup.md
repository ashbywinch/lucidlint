# Plan — the self-warning cleanup: 587 → 264, and what is left

The tool's own gate passes (`GATE: PASS`, 0 fails) with warnings, because
every rule that fires here is a judgement signal rather than a gate. The
cleanup drove the count down where the fix was mechanical and left the rest
as decisions. It is behaviour-preserving: no rule, threshold or message
changed. The original round compared against houses 353 / album 431; after
the agreed `undeclared-attribute` restore the current baselines are houses
420 / album 699, and both are unchanged by these rounds.

## Landed (branch `refactor/self-warnings`, stacked on the fix-engine split)

| kind | before | after | what changed |
|---|---|---|---|
| `detached-method` | 10 | 1 | static/associated fns; the survivor is a trait hook that must ignore its receiver |
| `unnamed-condition` | 3 | 0 | named predicates in `shared_path`, `global_state`, `special_case`, `module_splitting` |
| `broad-except` | 14 | 4 | narrowed to the errors handled; four pygit2 boundaries keep a why stating the blast radius |
| `standard` (duplicate-function) | 12 | 5 | shared bodies extracted; class delete and name rewrite became `Rewrite`'s edits |
| `magic-number` (Python) | 31 | 0 | every bar is a named class attribute (percentile band, churn seconds, record arity, block window, preview budget, bit width, saturation bars) |
| `magic-number` (Rust) | 98 | 0 | every threshold is a named `const` on the module that owns it (rank ladder, closure bars, decision counts, Dice floors, graph cuts, the complexity gate) |
| `loop-pipeline` (Python) | 24 | 21 | the five real collection builds became comprehensions |
| `duplicate-block` | 14 | 2 | transformer runs, compound flatten, literal types, diff walk, alias binding, field lookup, site note, branch question each have one home |
| `loop-pipeline` (Rust) | 376 | 277 | the collection builds chain (`extend(...map(...))`, `filter_map`), which also cleared the sequential-loop findings those loops carried |
| `conditional-polymorphism` | 3 | 0 | the two `isinstance` ladders read as kind tables (name binders, block binders) |

Verification: `make check`, 318 tests, `make self-check` (`GATE: PASS`),
houses 353 / album 431 (the baselines at that time) identical after every
commit.

## The loop family after #110 and #111

#110 settled the worklist/drain shapes: a loop whose condition removes from a
name the body writes is a traversal, and gets no verdict (issue #109 closed).
It also aligned the messages with the readable structures: one accumulator =
`map`/`filter_map` + `collect` or `fold`; several outputs from one pass = a
named accumulator struct + `fold`, or `itertools::multiunzip` /
`partition_map`; disjoint passes over a slice = leave them separate;
traversal = the worklist loop, named as the traversal. What remains is
254 findings, each a design decision rather than a mechanic:

| shape | count | what clearing it costs |
|---|---:|---|
| "mutates only out" (`loop-hoist`) | 110 | hoist the per-item step into a named helper (a domain noun per site), then combine or fold — 110 naming decisions |
| "N sequential loops" (`loop-sequence`) | 76 | merge two passes into one (folding one into the other's body) — the behaviour risk sits here |
| "mutates several accumulators" (`mutating-loop`) | 42 | one pass feeding several collections; the rule's advice is "make it a struct", i.e. a new type per site |
| Rust collection builds | 5 | guarded/nested walks with `continue` guards |
| Python collection builds | 21 | `setdefault(k, []).append(v)` group-bys and `|=` folds — no comprehension expresses them (the tool's own fixer declines) |

The hoists spread across the rules (`suppressions/mod.rs` 8,
`duplicate_module.rs` 5, `class_facts.rs` 5, `duplicate_def.rs` 5, …); the
sequences concentrate in `expr_walk.rs` 4, `state.rs` 4, `record_shape.rs` 3,
`rustscan/state.rs` 3, `sibling.rs` 3; the several-accumulator sites lead with
`main.rs` 5.

## The loop decision — for the owner

The owner's bar is that a loop is rewritten only where the rule's advice
holds: a pure per-item expression body. Worklist/drain shapes are settled by
#110 and stay as they are. The 26 collection builds are the shapes a
comprehension or chain does not express — only 2 of the 26 carry a `fix:`
directive — so they are not part of this decision. That leaves the 228
stateful/shape findings, with three options:

- **(a) Accept.** The scanner is a hand-written AST visitor, not a report
  generator. Many sites are the one-pass-over-scans idiom where a struct or a
  helper only renames what the loop already says. Record the acceptance in
  this plan; no config change, and the findings remain a standing signal.
- **(b) Do the 110 hoists.** One named helper per site is real naming work
  (~2 sessions); the 76 sequence merges carry the behaviour risk; the 42
  structs mostly rename a one-pass idiom. The worklist/drain shapes are out
  of scope either way.
- **(c) Scope the rule for `scanner/`** in a documented config with a why: a
  `.lucidlint.toml` `ignore` (or `[lucidlint.guidance]` text) for
  `loop-pipeline` under the scanner, so the rule stays armed everywhere else.
  The repo has no `.lucidlint.toml` today; this would be its first entry.

The decision is the owner's, not the next session's: (a) is silence, (b) is a
large grind, (c) is a policy change. Nothing else in this plan blocks on it.

## The small non-loop families (10)

Each is a rule-shape conflict rather than a smell:

| family | count | the decision |
|---|---:|---|
| `standard` | 5 | 3 libcst protocol hook pairs (`leave_Integer`/`leave_Float`, `leave_FunctionDef`/`leave_ClassDef`, `leave_IndentedBlock`) — extracting a shared body trips `middle-man`, and one generic `on_leave` hook is not type-checkable under the current libcst stubs; 2 Rust near-identical passes (`stmt_key`/`stmt_exact_key`, `return_values`/`ignored_test_findings`) with different concerns |
| `duplicate-block` | 2 | the argparse flag registration in `mechanical_fixes.py` and the rules-codegen `out.append` sequence in `gen-rules.py` — intentional twins |
| `bulk-suppression` | 1 | policy: 11 deliberate `swallow` whys in `source_walk.py`; the sanctioned route is one config guidance/ignore line, not 11 rewritten handlers |
| `detached-method` | 1 | an empty `syn` trait override (`visit_expr_closure`) that must ignore its receiver |
| `flat-folder` | 1 | name the subsets of `scanner/src/checks` (41 files); mechanical to move, but the subsets are a naming decision |

The three deferred message items in
[plan-usefulness-round-0-7-0](plan-usefulness-round-0-7-0.md) (JUDGEMENT
beside a runnable command, churn's unit, `stale-suppression`'s autofix) are
independent of this round and still open; none blocks it.

## This round — the last mechanical finding

`scanner/src/rustloops/mod.rs` fired `several-definitions` (3 top-level
definitions: `RustLoops`, `PipelineBuild`, `condition_drains`) after the
loop-shape refactor. `PipelineBuild`, `condition_drains` and
`DRAINING_METHODS` are consumed only by the per-function walk, so they moved
to `scanner/src/rustloops/walk.rs`: `condition_drains` is now a `LoopWalk`
method (it reads the walk's survivor set), `DRAINING_METHODS` a private const
beside it, and `PipelineBuild` the walk's own classification record. `mod.rs`
keeps the rule object (`RustLoops`) and its constants. No threshold, message
or behaviour changed: 265 → 264 warnings, the one removal this finding.

Verification for this round: `make check`, `make test` (519 scanner + 323
Python tests), `make self-check` (`GATE: PASS`, 264 warnings, 0 fails);
houses 420 / album 699 identical on both `(file, kind, line)` and
`(file, kind, message)`.

## The repo-wide merge, and the audit that closed it

`Pipeline::repo_wide_merge` (the LSP path) is one line:
`RepoScan::of(scans, root_s).by_file()`. `scanner/src/repo_scan.rs` owns the
corpus — the pools each repo-wide rule reads (`skeletons`, `definitions`,
`setters`, `references`, `suppressions`, `modules`, `forwarders`), each a
named field with a doc comment saying what the rule uses it for — and
`reconcile` asks each rule once, then applies that file's suppressions. The
definition and setter rows are records (`Definition`, `Setter`) rather than
`(rel, name, line, is_class)` tuples, so `unused` reads `definition.is_class`
instead of counting positions.

### Why the gate and the editor disagreed

The corpus started as the LSP's path; `fn main()` kept its own gather. That
made **five** copies of "which repo-wide families exist", none derived from
another:

| # | where | what it held |
|---|---|---|
| 1 | `RepoScan::findings` | 14 scan-derived kinds |
| 2 | `fn main()` | 12 of them, plus flat-folder and the families whose inputs only the gate has |
| 3 | `scan_corpus` (tests) | its own copy (12 kinds), with a different reconcile order |
| 4 | `drop_repo_wide_stale_artifacts` | 4 kinds (the LSP's per-buffer stale filter) |
| 5 | `rule_metadata.py` → `rules_gen.rs` | every kind's metadata, with no "where it runs" axis |

The move that broke it was `1c18dda`: `undeclared-attribute` went from a
per-file rule (every path got it for free) to a repo-wide family (each caller
must opt in). That commit updated the LSP merge and the test helper; `fn
main()` was six thousand lines away and kept the old list. Nothing could
fail: the registry only checks that every kind is *emitted somewhere*, and it
was (by `repo_scan.rs`), and no test asked whether the gate ran every family.
The gate skipped a documented **fail** rule for eleven weeks.

The fix removes the choice instead of testing it. `fn main()` owns no family
list: `RepoScan::reconcile` appends every scan-derived family to the gate's
findings, clearing the stale marker the per-file pass left for a marker it
consumes; `RepoScan::apply_suppressions` is the one final pass over the
families the gate composes itself; the test corpus is the same two calls.
Five lists became two — the corpus, and the external-input families whose
data only the gate has. `drop_repo_wide_stale_artifacts` reads
`RepoScan::FAMILIES`, with a test pinning it against the rule registry and a
test proving a deferred family's marker is not called stale in a buffer scan.

The earlier attempt to reuse the corpus produced 6 extra stale findings on
houses and 2 on album: `findings()` returned a finished set, so a marker the
corpus consumed had already been reported stale by the per-file pass and
nothing cleared that verdict. `reconcile` takes the caller's findings, so the
verdict is cleared where the marker is consumed.

### The audit table

| family / step | gate | corpus | verdict |
|---|---|---|---|
| `duplicate` (Python/Rust pools), `duplicate-module`, `forwarding-chain`, `unused`, `unused-setter`, `scattered-instance`, `shared-path`, `duplicate-expression`, `several-definitions`, `many-definitions`, `middle-man`, `sibling-value-class` | yes | yes | aligned; the gate now takes them from the corpus |
| `over-abstraction` | yes | **was no** | moved into the corpus — its input is each file's classes/imports, which the corpus already holds. No gate finding changes (0 on houses, album and here); the editor gains the diagnostic. |
| `undeclared-attribute` | **was no** | yes | restored, as agreed: fail severity. Houses 353 → 420 (+67), album 431 → 699 (+268) — every addition this family, nothing removed. lucidlint's own 55 members were declared first (`2aaa338`), so the gate stays green. |
| `flat-folder` | yes | no | gate-only by input: it counts the file inventory (modules + docs) under `--include-tests`, not scan data; the LSP's walk is `.py`-only. |
| final cumulative suppression pass | yes | per-family, plus `apply_suppressions` for the gate's own families | intentional: the corpus reconciles one family at a time (a marker's peel capacity is the family's), and `reconcile` clears the per-file stale marker it consumes. `apply_suppressions` is the door for the families the gate composes itself (flat-folder, graph, docs, churn, bulk). On houses and album no marker names one of those, so disabling it changes nothing there — verified; it is the rule for the next repo that has one. |
| `bulk-suppression` | yes | no | report family over the suppression census; the LSP renders no report. |
| Rust mod-graph cycles | yes | no | needs `RustScan.mod_decls/uses`, which `FileScan` does not carry; the LSP's repo walk is Python-only. |
| graph families (`high-risk`, cycles, `layer-mix`, `folder-mix`, `cohesion`) | yes | no | need the orchestrator's versioned graph contract. |
| docs integrity | yes (`--docs`) | no | needs the docs root and the gitignored list. |
| `hotspot`, `churn-untested` | yes (`--churn`, `--coverage`) | no | need git churn and the coverage snapshot. |
| `complexity` | yes | no | per-file cc; the LSP emits its own complexity diagnostic. |

Verification after every step: `make check`, the Python and Rust suites,
`make self-check` (`GATE: PASS`, 265 warnings at the close of the audit, 0
fails; 264 after this round's move), and a `(file, kind, line)` plus
`(file, kind, message)` finding-set comparison against the snapshot taken
before the change.

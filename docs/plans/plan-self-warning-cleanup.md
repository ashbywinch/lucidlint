# Plan — the self-warning cleanup: 587 → 265, and what is left

The tool's own gate passes (`GATE: PASS`, 0 fails) with warnings, because
every rule that fires here is a judgement signal rather than a gate. This
round drove the count down where the fix was mechanical and left the rest
as decisions. It is behaviour-preserving: no rule, threshold or message
changed, and houses 353 / album 431 keep identical finding sets.

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
houses 353 / album 431 identical after every commit.

## What is left — and why each removal needs a decision

**1a. The loop family is filed upstream.** The `sequential loops` and
`mutates only out` messages prescribe rewrites that Rust's own guidance
rejects: Effective Rust item 9 says to keep a large or multifunctional body
explicit and never force a loop into an iterator transform; chaining two
passes changes evaluation order (the create-then-join footgun); and separate
passes over an immutable slice are free because iterators are lazy and LLVM
fuses them. A worklist body that mutates the stack it pops from is a
traversal, not a fold — "the per-item step" is not a function of the item.
Filed as <https://github.com/ashbywinch/lucidlint/issues/109> with the counts,
the citations, and a third item: the message advertises `— fix: loop-pipeline`
on 17 findings the fixer refuses (`setdefault(…).add(…)` group-bys). The
readable structures the message should name: one accumulator =
`map`/`filter_map` + `collect` or `fold`; several outputs from one pass = a
named accumulator struct + `fold`, or `itertools::multiunzip` /
`partition_map`; disjoint passes over a slice = leave them separate;
traversal = the worklist loop, named as the traversal.

**1. `loop-pipeline` ×298 (277 Rust, 21 Python).** The 96 Rust collection
builds are done; what remains is the shapes where a rewrite is a design call:

| shape | count | what clearing it costs |
|---|---|---|
| "mutates only out" | 128 | hoist the per-item step into a named helper (a domain noun per site) and fold — 128 naming decisions in the scanner |
| "sequential loops share state" | 79 | merge two passes into one (folding one into the other's body) — the behaviour risk sits here |
| "mutates several accumulators" | 55 | one pass feeding several collections; the rule's advice is "make it a struct", i.e. a new type per site |
| Python collection builds | 21 | `setdefault(k, []).append(v)` group-bys and `|=` folds — no comprehension expresses them (the tool's own fixer declines) |
| Rust collection builds | 15 | the last few (nested walks with `continue` guards) |

The top files are `main.rs` 21, `suppressions/mod.rs` 17, `record_shape.rs` 11,
`sibling.rs` 10, `latent_state.rs` 9. Options: (a) accept — the scanner is a
hand-written AST visitor, not a report generator, and 55 of the sites are the
one-pass-over-scans idiom that a struct would only rename; (b) work the 128
hoists (each adds a named helper to the rule that owns it — real, ~2 sessions);
(c) scope the rule for `scanner/` in config with a why.

**2. `magic-number` — done.** Every Rust threshold is a named `const` on the
module that owns it, so the kind is empty.

**3. `flat-folder` ×1.** `scanner/src/checks` holds 41 modules. A split into
domain subfolders (`latent/`, `duplicate/`, `state/`, …) is mechanical but
touches every `mod`/`use` in the core; it needs a naming decision (what the
subsets are), not just effort.

**4. The Python residuals (11).** Each is a rule-shape conflict rather than
a smell:
- `loop-pipeline` ×21: `dict.setdefault(k, []).append(v)` group-bys and
  `set |= …` accumulator folds — a comprehension cannot express them (the
  tool's own fixer declines with "needs input").
- `duplicate-block` ×3: argparse flag registration and the rules-codegen's
  `out.append` sequence.
- `standard` ×3: libcst's protocol forces `leave_Integer`/`leave_Float` and
  `leave_FunctionDef`/`leave_ClassDef` pairs with identical bodies; extracting
  a shared body trips `middle-man`, and one generic `on_leave` hook is not
  type-checkable under the current libcst stubs.
- `conditional-polymorphism` ×2: `isinstance` chains in `_bind_children` and
  `collect_stmt_names`; the honest fix is a kind→handler table (the repo's
  dispatch pattern), ~30 lines each.
- `detached-method` ×1: `visit_expr_closure` is an empty trait override whose
  only job is to skip closures — it cannot use its receiver.
- `bulk-suppression` ×1: 11 `swallow` whys repo-wide (threshold 10). Each is a
  deliberate best-effort handler; warning instead of passing does not satisfy
  the rule, so clearing it means changing handler shapes or a config-level
  suppression.

**5. The policy lever.** The repo has no `.lucidlint.toml`. The tool's own
bulk-suppression message says repeated identical whys are policy: a config
`ignore` (or `[lucidlint.guidance]` text) for a signal is the sanctioned way
to say "this rule is guidance here" once, instead of per site.

## Where it stopped

At the end of the cleanup round: 308 warnings, 0 fails. Every one of them was
a decision rather than a mechanic:

| what | count | the decision |
|---|---|---|
| loop hoists ("mutates only out") | 42 | name the per-item step for each rule that owns one |
| loop sequences ("N sequential loops") | 79 | merge two passes into one, or accept the two-pass read |
| one pass over several accumulators | 55 | make a struct per site (the rule's advice) or accept the pass |
| collection builds left | 36 (15 Rust, 21 Python) | the guarded/nested walks; the Python ones are group-bys no comprehension expresses |
| `standard` | 5 | 3 libcst protocol hook pairs (extraction trips `middle-man`) and 2 Rust passes with different concerns |
| `duplicate-block` | 2 | the loop-plan guard shape and the rules-codegen emit pattern |
| `bulk-suppression` | 1 | policy: 11 deliberate `swallow` whys |
| `detached-method` | 1 | an empty `syn` trait override that must ignore its receiver |
| `flat-folder` | 1 | name the subsets of `scanner/src/checks` (41 files) |

Nothing above can be removed without someone saying what the code should say
instead — a helper name, a merged pass, a struct, a suppression, a folder.
That is the tool doing its job: the remaining findings are judgement.

The repo-wide audit round below took the total to **265** (254
`loop-pipeline`, 5 `standard`, 2 `duplicate-block`, 1 `bulk-suppression`, 1
`several-definitions`, 1 `detached-method`, 1 `flat-folder`): it deleted the gate's duplicate gather,
and the loops that lived in it. No remaining finding changed meaning.

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
`make self-check` (`GATE: PASS`, 265 warnings, 0 fails), and a
`(file, kind, line)` plus `(file, kind, message)` finding-set comparison
against the snapshot taken before the change.

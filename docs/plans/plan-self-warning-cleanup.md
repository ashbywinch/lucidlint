# Plan — the self-warning cleanup: 587 → 508, and what is left

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
| `duplicate-block` | 14 | 3 | transformer runs, compound flatten, literal types, diff walk, alias binding, field lookup, site note, branch question each have one home |

Verification: `make check`, 318 tests, `make self-check` (`GATE: PASS`),
houses 353 / album 431 identical after every commit.

## What is left — and why each removal needs a decision

**1. `loop-pipeline` ×376 in the Rust scanner** (96 "builds a collection",
129 "mutates only out", 96 "sequential loops share state", plus 55 smaller
judgement shapes; the top files are `main.rs` 29, `latent_state.rs` 19,
`suppressions/mod.rs` 18, `expr_walk.rs` 17, `tuple_record.rs` 16). The core
is a hand-written AST walker (`syn`/`ruff` visitors); loops that mutate one
accumulator are the domain's shape there. The two measurable sub-families:
the 96 collection builds convert one-for-one to `extend(...map(...))`; the 96
"sequential loops" are *pairs* of loops that would need merging into one pass,
which is where the behaviour risk sits. Options: (a) accept — the scanner is a
parser, not a report generator; (b) rewrite the 96 builds first (safe, ~1
session) and judge the sequences after; (c) scope the rule for `scanner/` in
config with a why.

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

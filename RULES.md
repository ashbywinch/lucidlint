# lucidlint rule reference

Every finding is one of two severities:

| Severity | Meaning |
|---|---|
| **fail** | Blocks the gate (`GATE: FAIL`) unless baselined |
| **warn** | Reported but never blocks the gate — fix if you want, but the gate passes |

## Suppression

Any finding can be silenced with a `lucidlint: ignore` comment on its line
or within a few lines above it (a 3-line window — one decorator or stacked
comment may intervene), provided you write a *reason*:

```python
# lucidlint: ignore magic-number the gate threshold — this literal is the defined limit
MAX_RETRIES = 3
```

A suppression without a why is itself a finding. Every `# type: ignore` /
`#[allow(...)]` / `#[ignore]` follows the same rule — the gate checks that a
reason accompanies it. A suppression naming a FAMILY also covers its variant
kinds (`ignore latent-class <why>` exempts `closures`, `partition`, and
`strewing` findings — the family collapse is `final_kind`'s, mirrored by
`FAMILY_VARIANTS` in common.rs).

A multi-line suppression comment must put its `lucidlint: ignore` marker on
the **last** line, within the window above the code; a marker farther up
never matches and is reported stale.

## Adding a finding family

Every finding has two identifiers — don't conflate them:

- **`signal`** — the raw family kind (e.g. `magic-number`). This is what
  suppressions match on: `lucidlint: ignore <signal> <why>`, config
  `ignore = ["<signal>"]`, `group:<name>` membership, and baseline
  identity.
- **`kind`** — the display kind in reports, `final_kind`'s output. Families
  without a named bucket collapse to `standard` (the message explains the
  rule).

A new family is registered in **one place**: a `Rule` in
`rule_metadata.py`'s `CATALOG` (kind, severity, config/display group,
languages, description, the display bucket — `None` for the kind's own
bucket, `"standard"` to collapse, or a family name shared with its
variants). Then:

1. Emit the finding in the scanner (`finding("<kind>", "<severity>", ...)`
   or the rustscan `finding` helper) — the drift gate fails if the catalog
   entry has no emission or the severities disagree, so the emission is the
   one thing that lives with the rule logic, not the metadata.
2. Run `make rules` — it regenerates RULES.md AND
   `scanner/src/rules_gen.rs` (FAMILY_KINDS, STANDARD_KINDS,
   FAMILY_VARIANTS, `final_kind`, `rule_groups`), and the Python
   `RULE_GROUPS` derives from the same catalog at import. Every derived
   artifact shares the catalog as its source — there is no second
   registration point to forget (this is the review-log B6 lesson: the
   family/variant map was a hand-maintained second list and `strewing`
   drifted out of it).

If the family has a fix (mechanical or structural), the finding MESSAGE
ends with the machine-parseable directive `— fix: <kind> [--name <N>]`;
the output layer rewrites it into the FULL command
(`lucidlint fix --kind <kind> --file <file> --line <line> [--name <N>]`)
so the agent is told the exact invocation (R27).

Then: unit tests for the scanner path, an orchestrator test if the family
changes gate behavior, a suppression test (`lucidlint: ignore` + config
`ignore`), and `make self-check` — the tool scans its own repo, so the
house code must be clean under the new family (or the finding baselined
with a why).

<!-- RULES-GENERATED:START -->

## Group 1: Architecture & design

&dagger; *More likely to conflict with existing conventions. The threshold values (15 CC, 120 lines, 5 fields, 0.9 dice) are opinionated — adjust by suppressing specific findings with a why.*

| Rule | Severity | Language | What it checks |
|---|---|---|---|
| **complexity** | fail | Both | Cyclomatic complexity ≥ 15 (radon-equivalent rules: `if`/`elif` count, `match` arms minus wildcard, `&&`/`||`, ternary, loops, `assert!`, closures +0 walked) — Extract Function: `lucidlint fix --kind extract-method --file <F> --line <L>` previews the best self-contained seam (placeholder name — the extracted function is private by construction, so the fix underscores it); apply with `--name <N>` (the name IS the commitment — no `--confirm`). |
| **long-param-list** | fail | Both | A function with > 5 parameters (receiver/`self` excluded) — introduce a parameter object. |
| **large-function** | fail | Both | Function spans ≥ 120 lines — split it: one rule per function. |
| **closures → latent-class** | fail | Both | A function defining ≥2 inner functions/closures (≥15 CC *or* ≥60 line span) — the nested structure is a class waiting to be extracted. |
| **partition → latent-class** | fail | Python | The field-partition variant of latent-class: free functions partition a struct's fields (each touches a disjoint subset) — the fields and their functions belong together as a class. |
| **strewing → latent-class** | fail | Both | ≥3 free functions sharing the same leading parameter — they share data, they're a class. |
| **misplaced-method → latent-class** | fail | Python | A module-level function called from a method with `self.<attr>` arguments matching every parameter — the class already holds the data, so the function is that class's method in exile; move it onto the class. |
| **assembly-class → latent-class** | fail | Python | A function assembles >=3 structures by threading the same data through module-level functions whose outputs feed each other's inputs — a class in waiting: the threaded state belongs on an object, the module functions are its methods. |
| **tuple-record → latent-class** | fail | Python | A dict whose values are same-arity tuples read with constant integer indexes — an anonymous record; make it a class (the record's fields are the tuple positions). |
| **data-clump → latent-class** | fail | Python | >=3 module functions sharing the same parameter pair — the pair travels together, so it is a data clump; introduce a parameter object that holds it. |
| **feature-envy → latent-class** | fail | Python | A method reads another object's fields more than its own state — the logic belongs on that object (feature envy); move the computation onto the envied class as a method. |
| **undeclared-attribute** | fail | Python | A class must DECLARE its members — annotated in the class body, in __slots__, or in __init__ — not assign them quietly in member functions. A plain `self.x = ...` in __init__ or any member assigned outside the constructor without a declaration is a finding: declare it. |
| **god-class → latent-class** | **warn** | Python | A class with >=20 methods, or >=12 methods over >=250 lines — a large class. A review signal, not a split order: split only where the partition rule finds field-disjoint method groups. |
| **duplicate-field → latent-class** | fail | Python | A class and a class it CONTAINS both hold the same >=2 fields — duplicated domain state across a containment edge; one source of truth should own it. |
| **module-cohesion** | fail | Graph | A file whose nodes split across ≥2 graph communities (each ≥2 nodes) holds several sub-domains — split the module at the domain seams. |
| **record-shape** | fail | Both | A function takes a struct/class with ≥5 fields and no methods — the struct's rules belong as methods on it. |
| **detached-method** | **warn** | Both | A method that never touches its receiver — a classmethod should always use `cls`; a plain method should use `self` or move out — it doesn't use instance state; make it a `@staticmethod`/associated fn or move it out of the class. |
| **duplicate** | **warn** | Both | Dice similarity ≥ 0.9 (structural skeleton bigrams) — copy-paste; extract the shared logic. |
| **layer-mix** | fail | Graph | A file calls into multiple architectural layers (determined via the code-review-graph contract) — files belong in one layer. |
| **folder-mix** | fail | Graph | Files in a directory are split across graph communities — they belong together. |

## Group 2: Style & correctness

| Rule | Severity | Language | What it checks |
|---|---|---|---|
| **magic-number** | **warn** | Both | Numeric literal (outside 0/1/2) used as an operand — name it as a constant. |
| **debug-artifact** | fail | Both | `dbg!()` / `.unwrap()` / `.expect()` in production Rust; `breakpoint()` in production Python — debugging left in. |
| **noop-statement** | fail | Both | Expression statement that discards its value (`x;`, `a + b;`) — dead statement. |
| **unreachable** | fail | Both | Statement after an unconditional `return`/`break`/`continue`/`panic!` — dead code is deleted. |
| **vague-name** | fail | Both | Type ending in Manager, Handler, Store, Repository, Controller, Utils, or Info with significant size/methods — the domain concept should name it. |
| **class-module** | fail | Python | A Python module holding exactly one class whose name doesn't match the filename — rename the file to match. |
| **builtin-shadow** | fail | Python | A variable/parameter that shadows a Python builtin (`list`, `dict`, `str`, `id`...). |
| **broad-except** | **warn** | Python | Bare `except:` — catch specific exceptions. |
| **boolean-arg** | fail | Both | A boolean literal passed as a call argument (`connect(host, True)`) — name the flag at the call site. |
| **positional-literals** | **warn** | Both | A call passing ≥2 literals of the same kind positionally (`set_limits(10, 20)`) — a swapped argument is a silent bug; use keyword arguments. |
| **swallow** | fail | Both | A catch that neither re-raises nor exits with control flow (no return/break/continue); in Rust, a `Result`/`Option` discarded with `let _ =` — the error vanishes, re-raise or handle it. |
| **inline-import** | fail | Python | `import` inside a function body (Python) — imports belong at module top. |
| **private-import** | fail | Both | Importing an underscore-prefixed symbol from another module. |
| **global-state** | fail | Both | Module-level mutable container mutated inside a function — put state in a class. |
| **unused** | **warn** | Python | A function defined in production code that's never referenced anywhere in the repo (same-file references count — Python only). |
| **duplicate-def** | fail | Both | A module-scope def/class/import that shadows an earlier module-scope binding of the same name — the later definition wins legally, but it is a shadowing hazard (dispatch or edit mistake); rename one. |
| **restating-docstring** | **warn** | Both | A docstring whose content words all appear in the body's own tokens — it restates the code; name the concept instead. |
| **duplicate-block** | **warn** | Both | An identical statement block (≥3 statements) appearing twice in one function — duplicated work (an edit mistake?); delete the second copy. |
| **import-cycle** | fail | Both | Circular imports — restructure modules. |
| **docs-link** | fail | Both | An internal MD link or backticked path does not resolve to an existing file. |
| **docs-undiscoverable** | fail | Both | A doc file is not reachable from `AGENTS.md` (the repo's doc index) via the link graph. |

## Group 3: Test discipline

| Rule | Severity | Language | What it checks |
|---|---|---|---|
| **monkeypatch** | fail | Python | `monkeypatch`/`unittest.mock.patch` — prefer dependency injection. |
| **skipif** | fail | Both | `@pytest.mark.skipif` on environment presence (`os.environ`, `sys.platform`, etc.), a bare `@pytest.mark.skip`, or `#[test] #[ignore]` — a skipped test rots; fake the dependency instead. |
| **fakefs** | fail | Both | Real filesystem I/O (`open`, `pathlib.Path`) in a test without `pyfakefs` — tests fake the filesystem. |
| **no-assert-test** | fail | Both | A test function with no assertion anywhere in its body — it can never fail. |

## Group 4: Suppression discipline

| Rule | Severity | Language | What it checks |
|---|---|---|---|
| **suppression** | fail | Both | `lucidlint: ignore <signal>` with no explanation — every exemption needs a why. |
| **type-ignore** | fail | Python | `# type: ignore` with no comment — a suppression is itself a finding; explain why the checker is wrong. |
| **allow-reason** | fail | Rust | `#[allow(...)]` / `#[expect(...)]` with no reason comment on the line or the line above. |
| **noqa** | fail | Python | `# noqa` / `# pragma: no cover` with no explanation — a suppression is itself a finding. |
| **stale-suppression** | fail | Both | A `lucidlint: ignore` / `ignore-file` that no longer suppresses anything — remove it. |

## Group 4.5: Refactoring advice

&dagger; *(all warn — detection-only) These detect the code SHAPE a Fowler refactoring targets. The fix is named in the message for the agent to hand-apply (auto-fixes exist for magic-number, vague-name, and long-param-list; see the fix engine).*

| Rule | Severity | Language | What it checks |
|---|---|---|---|
| **guard-clauses** | **warn** | Python | ≥3 levels of if-in-if ("arrow code") — Replace Nested Conditional with Guard Clauses: invert to early returns. |
| **latent-visitor** | **warn** | Both | ≥2 operations dispatching over the same element family (`isinstance`/`type()` chains) — Replace Conditional with Visitor: elements accept a visitor with `visit_<Type>` methods. Chains this rule claims are exempt from conditional-polymorphism — one ruling per chain. |
| **conditional-polymorphism** | **warn** | Python | An if/elif chain of ≥4 arms dispatching on the same value — Replace Conditional with Polymorphism. |
| **special-case** | **warn** | Python | ≥3 repeated `None`/empty checks on one name — Introduce Special Case. |
| **middle-man** | **warn** | Python | A method that only forwards (`return self.x.y(...)`) — Remove Middle Man. |
| **unused-setter** | **warn** | Python | A `set_*` method or property setter never referenced — Remove Setting Method. |
| **loop-pipeline** | **warn** | Python | A loop whose body is only a collection mutation — Replace Loop with Pipeline: use a comprehension. |

## Group 5: Hotspot & risk (graph-based)

These rules require the optional `code-review-graph` tool (installed separately or via `pip install code-review-graph`). Without it they degrade silently.

| Rule | Severity | Language | What it checks |
|---|---|---|---|
| **hub-file** | fail | Graph | A file with ≥150 incoming or outgoing call/import edges — central module that may need splitting. |
| **high-risk** | fail | Graph | A function with >10 callers, no test coverage, and/or a security-related name (`auth`, `login`, `token`, `sql`...). |
| **hotspot** | fail | Graph | A file in the top N% by churn with a function at ≥15 CC — volatile code that needs refactoring. |
| **churn-untested** | fail | Graph | A file in the top N% by churn with no test coverage — volatile and unverified. |
| **over-abstraction** | fail | Graph | An abstract base class (Python ABC) with exactly one concrete subclass — the abstraction doesn't earn its keep. |

## Group 6: Cross-cutting

| Rule | Severity | Language | What it checks |
|---|---|---|---|
| **standard** | — | — | Catch-all for findings that don't fit the named families above. The finding's message explains what's wrong. |

<!-- RULES-GENERATED:END -->

## The `--include-tests` flag

By default, test files (`tests/`, `test_`-prefixed, `#[cfg(test)]` items in Rust) are scanned for Group 3 rules only — their magic numbers, complexity, and architecture findings are suppressed (tests have different standards). Pass `--include-tests` to scan everything in test files.
# lucidlint rule reference

Every finding is one of two severities:

| Severity | Meaning |
|---|---|
| **fail** | Blocks the gate (`GATE: FAIL`) unless baselined |
| **warn** | Reported but never blocks the gate — fix if you want, but the gate passes |

## Suppression

Any finding can be silenced with a `code-health: ignore` comment on its line or the line above, provided you write a *reason*:

```python
# code-health: ignore magic-number the gate threshold — this literal is the defined limit
MAX_RETRIES = 3
```

A suppression without a why is itself a finding. Every `# type: ignore` / `#[allow(...)]` / `#[ignore]` follows the same rule — the gate checks that a reason accompanies it.

The first two groups are the ones most likely to collide with existing codebase conventions. Each is individually suppressible.

## Group 1: Architecture & design &dagger;

| Rule | Severity | What it checks |
|---|---|---|
| **complexity** | fail | Cyclomatic complexity ≥ 15 (radon-equivalent rules: `if`/`elif` count, `match` arms minus wildcard, `&&`/`||`, ternary, loops, `assert!`, closures +0 walked) |
| **large-function** | fail | Function spans ≥ 120 lines — split it: one rule per function |
| **closures → latent-class** | fail | A function defining ≥2 inner functions/closures (≥15 CC *or* ≥60 line span) — the nested structure is a class waiting to be extracted |
| **strewing** | fail | ≥3 free functions sharing the same leading parameter — they share data, they're a class |
| **record-shape** | fail | A function takes a struct/class with ≥5 fields and no methods — the struct's rules belong as methods on it |
| **duplicate** | fail | Dice similarity ≥ 0.9 (structural skeleton bigrams) — copy-paste; extract the shared logic |
| **layer-mix** | fail | A file calls into multiple architectural layers (determined via the code-review-graph contract) — files belong in one layer |
| **folder-mix** | fail | Files in a directory are split across graph communities — they belong together |

&dagger; *More likely to conflict with existing conventions. The threshold values (15 CC, 120 lines, 5 fields, 0.9 dice) are opinionated — adjust by suppressing specific findings with a why.*

## Group 2: Style & correctness

| Rule | Severity | What it checks |
|---|---|---|
| **magic-number** | **warn** | Numeric literal (outside 0/1/2) used as an operand — name it as a constant |
| **noop-statement** | fail | Expression statement that discards its value (`x;`, `a + b;`) — dead statement |
| **unreachable** | fail | Statement after an unconditional `return`/`break`/`continue`/`panic!` — dead code is deleted |
| **vague-name** | fail | Type ending in Manager, Handler, Store, Repository, Controller, Utils, or Info with significant size/methods — the domain concept should name it |
| **class-module** | fail | A Python module holding exactly one class whose name doesn't match the filename — rename the file to match |
| **shadow** | fail | A variable/parameter that shadows a Python builtin (`list`, `dict`, `str`, `id`...) |
| **broad-except** | **warn** | Bare `except:` — catch specific exceptions |
| **inline-import** | fail | `import` inside a function body (Python) — imports belong at module top |
| **private-import** | fail | Importing an underscore-prefixed symbol from another module |
| **global-state** | fail | Module-level mutable container mutated inside a function — put state in a class |
| **unused** | fail | A function defined in production code that's never referenced from any other prod file (Python only) |
| **import-cycle** | fail | Circular imports — restructure modules |
| **docs-link** | fail | An internal MD link or backticked path does not resolve to an existing file |
| **docs-undiscoverable** | fail | A doc file is not reachable from `AGENTS.md` (the repo's doc index) via the link graph |

## Group 3: Test discipline

| Rule | Severity | What it checks |
|---|---|---|
| **monkeypatch** | fail | `monkeypatch`/`unittest.mock.patch` — prefer dependency injection |
| **skipif** | fail | `@pytest.mark.skipif` on environment presence (`os.environ`, `sys.platform`, etc.) — a skipped test rots; fake the dependency instead |
| **fakefs** | fail | Real filesystem I/O (`open`, `pathlib.Path`) in a test without `pyfakefs` — tests fake the filesystem |
| **ignored-test** | fail | `#[test] #[ignore]` — a parked test rots; fix it or delete it |

## Group 4: Suppression discipline

| Rule | Severity | What it checks |
|---|---|---|
| **suppression** | fail | `code-health: ignore <signal>` with no explanation — every exemption needs a why |
| **type-ignore** | fail | `# type: ignore` with no comment — a suppression is itself a finding; explain why the checker is wrong |
| **allow-reason** (Rust) | fail | `#[allow(...)]` / `#[expect(...)]` with no reason comment on the line or the line above |

## Group 5: Hotspot & risk (graph-based)

These rules require the optional `code-review-graph` tool (installed separately or via `pip install code-review-graph`). Without it they degrade silently.

| Rule | Severity | What it checks |
|---|---|---|
| **hub-file** | fail | A file with ≥150 incoming or outgoing call/import edges — central module that may need splitting |
| **high-risk** | fail | A function with >10 callers, no test coverage, and/or a security-related name (`auth`, `login`, `token`, `sql`...) |
| **hotspot** | fail | A file in the top N% by churn with a function at ≥15 CC — volatile code that needs refactoring |
| **abstraction** | fail | An abstract base class (Python ABC) with exactly one concrete subclass — the abstraction doesn't earn its keep |
| **folder-mix** | fail | (listed above — graph community–driven) |

## Group 6: Cross-cutting

| Rule | What it means |
|---|---|
| **standard** | Catch-all for findings that don't fit the named families above. The finding's message explains what's wrong. |

## The `--include-tests` flag

By default, test files (`tests/`, `test_`-prefixed, `#[cfg(test)]` items in Rust) are scanned for Group 3 rules only — their magic numbers, complexity, and architecture findings are suppressed (tests have different standards). Pass `--include-tests` to scan everything in test files.
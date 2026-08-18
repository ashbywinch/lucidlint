"""Canonical rule metadata — the single source for the RULES.md rule tables.

The group tables in RULES.md are GENERATED from this module by
`scripts/gen-rules.py` (`make rules`), so the list of rules cannot drift
from the code: the gate (`test_rules_md_is_generated`) fails if the
generated tables no longer match this file, and the registration tests
fail if any kind the scanner emits lacks an entry here.

Each entry: (kind, display_name, display_group, languages, severity,
description). `kind` must be a member of the scanner's FAMILY_KINDS.
`display_name` is the table name (defaults to the kind; used for rows
that show an alias, e.g. closures → latent-class). `display_group` is the
RULES.md section; `languages` is the human-readable language column;
`severity` is checked against the scanner's emitted severity where that
is parseable (kind + severity literals in one Finding construction).
"""

# display groups in RULES.md order — (name, header suffix, intro note)
GROUP_INFO = {
    "architecture": (
        "Group 1: Architecture & design",
        "&dagger; *More likely to conflict with existing conventions. The "
        "threshold values (15 CC, 120 lines, 5 fields, 0.9 dice) are "
        "opinionated — adjust by suppressing specific findings with a why.*",
    ),
    "style": ("Group 2: Style & correctness", None),
    "test-discipline": ("Group 3: Test discipline", None),
    "suppression": ("Group 4: Suppression discipline", None),
    "advice": (
        "Group 4.5: Refactoring advice",
        "&dagger; *(all warn — detection-only) These detect the code SHAPE a "
        "Fowler refactoring targets. The fix is named in the message for the "
        "agent to hand-apply (auto-fixes exist for magic-number, vague-name, "
        "and long-param-list; see the fix engine).*",
    ),
    "graph": (
        "Group 5: Hotspot & risk (graph-based)",
        "These rules require the optional `code-review-graph` tool (installed "
        "separately or via `pip install code-review-graph`). Without it they "
        "degrade silently.",
    ),
    "cross-cutting": ("Group 6: Cross-cutting", None),
}

# (kind, display_name, display_group, languages, severity, description)
RULES = [
    # ---- architecture
    ("complexity", None, "architecture", "Both", "fail",
     "Cyclomatic complexity ≥ 15 (radon-equivalent rules: `if`/`elif` count, "
     "`match` arms minus wildcard, `&&`/`||`, ternary, loops, `assert!`, "
     "closures +0 walked) — Extract Function: `lucidlint fix --kind "
     "extract-method --file <F> --line <L>` previews the best self-contained "
     "seam (placeholder name — the extracted function is private by "
     "construction, so the fix underscores it); apply with `--name <N>` "
     "(the name IS the commitment — no `--confirm`)."),
    ("long-param-list", None, "architecture", "Both", "fail",
     "A function with > 5 parameters (receiver/`self` excluded) — introduce "
     "a parameter object."),
    ("large-function", None, "architecture", "Both", "fail",
     "Function spans ≥ 120 lines — split it: one rule per function."),
    ("closures", "closures → latent-class", "architecture", "Both", "fail",
     "A function defining ≥2 inner functions/closures (≥15 CC *or* ≥60 line "
     "span) — the nested structure is a class waiting to be extracted."),
    ("partition", "partition → latent-class", "architecture", "Python", "fail",
     "The field-partition variant of latent-class: free functions partition "
     "a struct's fields (each touches a disjoint subset) — the fields and "
     "their functions belong together as a class."),
    ("strewing", None, "architecture", "Both", "fail",
     "≥3 free functions sharing the same leading parameter — they share "
     "data, they're a class."),
    ("record-shape", None, "architecture", "Both", "fail",
     "A function takes a struct/class with ≥5 fields and no methods — the "
     "struct's rules belong as methods on it."),
    ("detached-method", None, "architecture", "Both", "warn",
     "A method that never touches its receiver — a classmethod should "
     "always use `cls`; a plain method should use `self` or move out — it "
     "doesn't use instance state; make it a `@staticmethod`/associated fn "
     "or move it out of the class."),
    ("duplicate", None, "architecture", "Both", "warn",
     "Dice similarity ≥ 0.9 (structural skeleton bigrams) — copy-paste; "
     "extract the shared logic."),
    ("layer-mix", None, "architecture", "Graph", "fail",
     "A file calls into multiple architectural layers (determined via the "
     "code-review-graph contract) — files belong in one layer."),
    ("folder-mix", None, "architecture", "Graph", "fail",
     "Files in a directory are split across graph communities — they belong "
     "together."),
    # ---- style
    ("magic-number", None, "style", "Both", "warn",
     "Numeric literal (outside 0/1/2) used as an operand — name it as a "
     "constant."),
    ("debug-artifact", None, "style", "Both", "fail",
     "`dbg!()` / `.unwrap()` / `.expect()` in production Rust; "
     "`breakpoint()` in production Python — debugging left in."),
    ("noop-statement", None, "style", "Both", "fail",
     "Expression statement that discards its value (`x;`, `a + b;`) — dead "
     "statement."),
    ("unreachable", None, "style", "Both", "fail",
     "Statement after an unconditional `return`/`break`/`continue`/`panic!` "
     "— dead code is deleted."),
    ("vague-name", None, "style", "Both", "fail",
     "Type ending in Manager, Handler, Store, Repository, Controller, Utils, "
     "or Info with significant size/methods — the domain concept should "
     "name it."),
    ("class-module", None, "style", "Python", "fail",
     "A Python module holding exactly one class whose name doesn't match "
     "the filename — rename the file to match."),
    ("builtin-shadow", None, "style", "Python", "fail",
     "A variable/parameter that shadows a Python builtin (`list`, `dict`, "
     "`str`, `id`...)."),
    ("broad-except", None, "style", "Python", "warn",
     "Bare `except:` — catch specific exceptions."),
    ("boolean-arg", None, "style", "Both", "fail",
     "A boolean literal passed as a call argument (`connect(host, True)`) — "
     "name the flag at the call site."),
    ("positional-literals", None, "style", "Both", "warn",
     "A call passing ≥2 literals of the same kind positionally "
     "(`set_limits(10, 20)`) — a swapped argument is a silent bug; use "
     "keyword arguments."),
    ("swallow", None, "style", "Both", "fail",
     "A catch that neither re-raises nor exits with control flow (no "
     "return/break/continue); in Rust, a `Result`/`Option` discarded with "
     "`let _ =` — the error vanishes, re-raise or handle it."),
    ("inline-import", None, "style", "Python", "fail",
     "`import` inside a function body (Python) — imports belong at module "
     "top."),
    ("private-import", None, "style", "Both", "fail",
     "Importing an underscore-prefixed symbol from another module."),
    ("global-state", None, "style", "Both", "fail",
     "Module-level mutable container mutated inside a function — put state "
     "in a class."),
    ("unused", None, "style", "Python", "fail",
     "A function defined in production code that's never referenced from "
     "any other prod file (Python only)."),
    ("import-cycle", None, "style", "Both", "fail",
     "Circular imports — restructure modules."),
    ("docs-link", None, "style", "Both", "fail",
     "An internal MD link or backticked path does not resolve to an "
     "existing file."),
    ("docs-undiscoverable", None, "style", "Both", "fail",
     "A doc file is not reachable from `AGENTS.md` (the repo's doc index) "
     "via the link graph."),
    # ---- test discipline
    ("monkeypatch", None, "test-discipline", "Python", "fail",
     "`monkeypatch`/`unittest.mock.patch` — prefer dependency injection."),
    ("skipif", None, "test-discipline", "Both", "fail",
     "`@pytest.mark.skipif` on environment presence (`os.environ`, "
     "`sys.platform`, etc.), a bare `@pytest.mark.skip`, or `#[test] "
     "#[ignore]` — a skipped test rots; fake the dependency instead."),
    ("fakefs", None, "test-discipline", "Both", "fail",
     "Real filesystem I/O (`open`, `pathlib.Path`) in a test without "
     "`pyfakefs` — tests fake the filesystem."),
    ("no-assert-test", None, "test-discipline", "Both", "fail",
     "A test function with no assertion anywhere in its body — it can never "
     "fail."),
    # ---- suppression discipline
    ("suppression", None, "suppression", "Both", "fail",
     "`lucidlint: ignore <signal>` with no explanation — every exemption "
     "needs a why."),
    ("type-ignore", None, "suppression", "Python", "fail",
     "`# type: ignore` with no comment — a suppression is itself a finding; "
     "explain why the checker is wrong."),
    ("allow-reason", None, "suppression", "Rust", "fail",
     "`#[allow(...)]` / `#[expect(...)]` with no reason comment on the line "
     "or the line above."),
    ("noqa", None, "suppression", "Python", "fail",
     "`# noqa` / `# pragma: no cover` with no explanation — a suppression "
     "is itself a finding."),
    ("stale-suppression", None, "suppression", "Both", "fail",
     "A `lucidlint: ignore` / `ignore-file` that no longer suppresses "
     "anything — remove it."),
    # ---- refactoring advice (all warn — detection-only)
    ("guard-clauses", None, "advice", "Python", "warn",
     "≥3 levels of if-in-if (\"arrow code\") — Replace Nested Conditional "
     "with Guard Clauses: invert to early returns."),
    ("latent-visitor", None, "advice", "Both", "warn",
     "≥2 operations dispatching over the same element family "
     "(`isinstance`/`type()` chains) — Replace Conditional with Visitor: "
     "elements accept a visitor with `visit_<Type>` methods. Chains this "
     "rule claims are exempt from conditional-polymorphism — one ruling per "
     "chain."),
    ("conditional-polymorphism", None, "advice", "Python", "warn",
     "An if/elif chain of ≥4 arms dispatching on the same value — Replace "
     "Conditional with Polymorphism."),
    ("special-case", None, "advice", "Python", "warn",
     "≥3 repeated `None`/empty checks on one name — Introduce Special "
     "Case."),
    ("middle-man", None, "advice", "Python", "warn",
     "A method that only forwards (`return self.x.y(...)`) — Remove Middle "
     "Man."),
    ("unused-setter", None, "advice", "Python", "warn",
     "A `set_*` method or property setter never referenced — Remove Setting "
     "Method."),
    ("loop-pipeline", None, "advice", "Python", "warn",
     "A loop whose body is only a collection mutation — Replace Loop with "
     "Pipeline: use a comprehension."),
    # ---- graph-based (require code-review-graph)
    ("hub-file", None, "graph", "Graph", "fail",
     "A file with ≥150 incoming or outgoing call/import edges — central "
     "module that may need splitting."),
    ("high-risk", None, "graph", "Graph", "fail",
     "A function with >10 callers, no test coverage, and/or a "
     "security-related name (`auth`, `login`, `token`, `sql`...)."),
    ("hotspot", None, "graph", "Graph", "fail",
     "A file in the top N% by churn with a function at ≥15 CC — volatile "
     "code that needs refactoring."),
    ("churn-untested", None, "graph", "Graph", "fail",
     "A file in the top N% by churn with no test coverage — volatile and "
     "unverified."),
    ("over-abstraction", None, "graph", "Graph", "fail",
     "An abstract base class (Python ABC) with exactly one concrete "
     "subclass — the abstraction doesn't earn its keep."),
]


def display_name_of(kind: str) -> str:
    for k, name, *_rest in RULES:
        if k == kind:
            return name or k
    return kind

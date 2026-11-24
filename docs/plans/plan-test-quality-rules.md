# Plan — test-quality rules and one-list parity (#125, #123, #124)

The owner's order: `test-asserts-source` (#125), then `code-in-literal`
(#123), then the one-list parity design (#124). The branch is
`feat/test-quality-rules`, off `origin/main`; the loop-hoist grind and the
loop-policy decision stay with the owner.

## #125 — a source assertion is not a behaviour test

`test-asserts-source` (Both, warn) reports an assertion whose subject is
**code text** — a code-shaped fragment or block in the assertion, or a name
holding code text (a code literal, an `include_str!` of a source file, or a
read of one). There is no fixture-name or origin heuristic: a code fragment is
about code wherever it came from, and the same rule applies to a fixer's
output — `fixed` is code text too.

The code is scoped to the language under test: a Python test is judged on
Python code, a Rust test on Rust code. A shell or HTML artefact read by a
Python test is outside the rule; its text is the artefact, not the Python
code under test.

The message:

> test '{function}' asserts on code text, not on the behaviour under test —
> a text match pins one spelling, so it passes even when the code does
> nothing. Keep the test: assert what the code actually does instead. Do
> not delete the test or weaken it to match the text.

Wiring: the Python pass runs with the test-only families (test paths, or
`--include-tests`); the Rust pass is a pre-pass over `#[test]` and
`#[cfg(test)]` code. `tests/fixtures/` is never scanned, so the rule's own
tests are fixtures — `asserts_source_bad.py` / `asserts_source_ok.py` and
their Rust pair — and assert the finding set, not the fixture text.

## #123 — code lives in a file, not a string literal

`code-in-literal` (Both, warn) reports a string literal that holds a code
block (a line that reads as a statement of `fn`/`def`/`class`/`let`/`impl`/
`use`/`import`/`for`/`if`/`match`, at least two non-empty lines) or a snippet
passed as the first argument of a scan/compile/exec helper. The shape test
is deliberately narrow: the keyword line must carry the statement's own
punctuation (a call, block, binding, or module path), so prose that starts
with the same word — "class in waiting", "if you want:" — is not code.
Excluded: docstrings, one-line messages, regex/format strings, `include_str!`
and the formatting macros.

Message: *this string is code a reader cannot lint or run; move it to a real
file under tests/fixtures/ and read the file.*

Migration: 470 literals across the scanner's Rust tests, the Python
orchestrator/fix-engine tests, and `scripts/` moved to 352 real files under
`tests/fixtures/code/`, read with `include_str!` or `code_fixture(...)`.
The 39 remaining initial matches were false positives the helper-argument
rule removed (`compile`'s filename/mode arguments and bare filenames passed
to `scan`); they were not suppressed. A fresh `--include-tests` scan of the
repository reports zero `code-in-literal` findings.

Helper recognition is two named sets on `CodeText`: bare scan/compile/exec
names, and qualified scan helpers only (`re.compile` stays a regex). Macro
arguments are outside the rule by design and the `my_macro!("fn …")` case is
pinned in the exclusion fixture.

## #124 — one list, derived

The rule catalog is the one registry. `Rule` no longer takes a `languages`
label: it takes `implementations` (`python`, `rust`, `graph`), and the
RULES.md column is a property — both code languages `Both`, one its name,
the graph contract `Graph`. A rule with no implementation cannot be
constructed, and there is no input that says `Both` independently of the
set. The generated artifacts (`RULES.md`, `rules_gen.rs`) keep deriving from
the catalog.

The shape corpus is `tests/parity/<rule>/<shape>/` with `python.py`,
`rust.rs`, and `expected.json`; a construct the language does not have is a
`<language>.not_applicable` file carrying the reason. `tests/test_rule_parity.py`
discovers the tree — no manifest — and fails on a missing language file, a
file for an unimplemented language, or findings that disagree with
`expected.json`.

Seed (loop family):

| shape | Python | Rust | expected |
|---|---|---|---|
| `build` | build | build | `loop-pipeline` in both |
| `guarded` | guarded | guarded | `loop-pipeline` in both |
| `self_feed` | self_feed | self_feed | silence in both |
| `chained_target` | chained_target | chained_target | silence in both |
| `traversal` | traversal | traversal | silence in both |
| `for_else` | for_else | `rust.not_applicable` | silence (Python only) |

The negative shapes are the point: the Rust self-feed and Python for-else
gaps are now visible in one corpus before a reviewer sees a PR.

## The gate now includes tests

`make self-check` runs
`lucidlint.py --repo . --include-tests --test-quality`: `--include-tests`
brings test files into the scan, and `--test-quality` judges them by the
test-quality signals (the full audit without it fails on test-only standards).
`tests/fixtures/` stays excluded by the source walk — it is intentionally
non-compliant input.

The code-text rule reports the repo's own code-text assertions rather than
exempting output text. Before migration:

| repo | `test-asserts-source` | what they were |
|---|---:|---|
| lucidlint | 163 | 160 fixer-output assertions in `tests/test_fix_engine.py`, 3 CLI fixer output assertions |
| houses | 0 | the 7 pre-scoping findings were shell/HTML/JS artefacts, outside the Python code under test |
| family-history-album | 3 | import-line assertions in `tests/test_architecture.py` |

All 163 lucidlint assertions are migrated in this PR: 130 now assert the
parsed action set changed (or stayed unchanged for a refusal), 30 assert the
output's AST structure, and 3 CLI tests assert the parsed call/keyword shape.
No test was deleted. The gate is back to 275 warnings, 0 fails, and the rule
reports zero findings under `--include-tests`.

## Verification

- `make check`, `make test` (334 pytest tests, 555 scanner tests),
  `make self-check` (`GATE: PASS`, 275 warnings — 261 loop-family; zero
  `test-asserts-source`).
- houses: on `origin/main`, 238 findings → 235 after removing the three
  stale `loop-pipeline` markers; album: 677 → 676 after removing the one.
  The before/after diff on `(file, kind, line)` and `(file, kind, message)`
  shows exactly the stale-suppression removals and no additions. Cleanup
  PRs: houses#231, the-loft#152.
- The branch build reports fewer corpus findings than the pinned 0.7.1
  release (houses 423 / album 693 in the handoff) because it includes the
  loop-shape fixes; the comparison above is branch-build before/after.

## Left open

- The loop hoists and the loop policy (owner).
- Add the other `Both` families to `tests/parity/`; the runner is ready and
  each new shape is one directory.
- The loop policy (owner), and the other test-quality families if any are
  added.

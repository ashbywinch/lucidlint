# Plan — test-quality rules and one-list parity (#125, #123, #124)

The owner's order: `test-asserts-source` (#125), then `code-in-literal`
(#123), then the one-list parity design (#124). The branch is
`feat/test-quality-rules`, off `origin/main`; the loop-hoist grind and the
loop-policy decision stay with the owner.

## #125 — a source assertion is not a behaviour test

`test-asserts-source` (Both, warn) follows the variable's origin inside a
test and reports an assertion on a value bound directly from a fixture or a
source read (`fixture(...)`, `_fixture`, `read_text`, Rust `include_str!`).
The result of the code under test (`out`, `fixed`, the findings) is an
output; the rule never binds it, so asserting on a fixer's or parser's output
stays legal even when that output is source text.

The message is the one the owner wrote:

> this test asserts on the source text it fed the code, not on the behaviour
> under test — a source assertion pins the input, so it passes even when the
> code does nothing. Assert the parsed finding/output instead.

Wiring: the Python pass runs with the test-only families (test paths, or
`--include-tests`); the Rust pass is a pre-pass over `#[test]` and
`#[cfg(test)]` code. `tests/fixtures/` is never scanned, so the rule's own
tests are fixtures — `asserts_source_bad.py` / `asserts_source_ok.py` and
their Rust pair — and assert the finding set, not the fixture text.

The origin model is one type, `SourceInputs`: a name is a fixture input when
its last unconditional binding was a source read, and every branch runs on a
clone whose writes are committed by an explicit must-source join. The join
intersects every reachable path — zero-iteration/body/orelse for a loop,
pre-try/body/handlers for a try, pre-match/cases for a match — so a fixture
load or an output rebind on only one path is not a finding: the message must
be true on every path. The keep and both drop directions are unit-tested in
the module.

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

The first run exposed 149 `test-asserts-source` findings, almost all the same
false positive: a test reads back the file the fixer wrote
(`out = p.read_text()` after `p.write_text(src)`, or
`fixed = (repo / "houses" / "app.py").read_text()`). A raw `read_text` is now
a source read only when its path names a fixtures location; a read of a file
the test or the code under test wrote is the OUTPUT the test legitimately
asserts. The suite is at zero `test-asserts-source` and zero
`code-in-literal` findings under `--include-tests`.

## Verification

- `make check`, `make test` (333 pytest tests, 551 scanner tests),
  `make self-check` (`GATE: PASS`, 275 warnings — 261 of them the loop
  family the owner is deciding on).
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

# Plan — the docs walk asks git (no filesystem walk, no unreadable-file crash)

Carried on the `fix/issue-42-export-exemption` branch alongside the
record-shape exemption (a separate change — see
`docs/plans/plan-issue-42-export-exemption.md`). Test-first; the two-repo
run is the acceptance instrument.

## Verified facts (grounding)

- `lucidlint --repo houses` crashes before scanning, identically for the
  0.6.7 release core and the local build:
  `FileNotFoundError: .../houses/.kilo/worktrees/exclusive-pincushion/.omp/rules/fail-fast.md`
  from `_gitignored_docs` (`lucidlint.py:515`, unguarded `read_text`). Five
  `.md` symlinks in that stale Kilo worktree dangle — each points three
  levels up into omp-config/rules, which resolves to nothing from
  `.kilo/worktrees/exclusive-pincushion/.omp/rules/`.
- `.kilo/worktrees/` IS gitignored, via `.git/info/exclude` (Kilo Code
  agent worktrees) — and that is why the function reads it: the walk exists
  to enumerate the repo's ignored docs so the core treats them as
  intentionally absent. `os.walk` does not consult git; it prunes a
  hand-maintained `_SKIP_DIRS` (no `.kilo`) and filters per file with
  pygit2 afterwards.
- The core only ever reads `docs/**` + `README.md` + `AGENTS.md`
  (`docs.rs::docs_findings`) and uses the ignored set for exactly two
  decisions: skip an ignored doc under `docs/`, treat a referenced path as
  intentionally absent. Measured: houses has 691 ignored `.md` and the album
  276, of which the part the core can act on (`docs/`) is **zero in both**.
  The rest are vendored READMEs (`.venv/lib`, `node_modules`), agent state
  (`.kilo/node_modules`, `.sisyphus/plans`, `.omp/local`), the graph wiki and
  `tmp/` — files no finding can come from.
- `.kilo/worktrees/exclusive-pincushion/` is itself a worktree: git reports
  it as ONE opaque entry and never descends it, while `os.walk` descended and
  read its 57 `.md` files.
- Walk cost is NOT the issue (houses 71 ms → 65 ms pruned; album 130 ms →
  108 ms): the defect is the unguarded read, and the waste is reading
  markdown the scan never parses.

## The change

1. `_git_ignored_md`: the ignored `.md` paths are `git ls-files --others
   --ignored --exclude-standard -z -- '*.md'` — one call (0.22 s in houses),
   no walk, no `_SKIP_DIRS` (deleted — it had no other user), and no descent
   into a nested checkout.
2. `_repo_md_paths`: the reference harvest reads the repo's OWN `.md` files
   — `git ls-files --cached --others --exclude-standard`, the enumeration
   `_py_files` uses — so every doc the repo owns is read, wherever it lives.
   Ignored docs are not the shipped doc set and a nested checkout is one
   opaque entry, so git enumerates neither; the harvest does NOT re-derive
   the core's scan roots (an earlier revision of this change did, which
   silently dropped references made from any non-ignored doc outside
   `docs/` + the two roots — a scope the harvest has no business deciding).
3. Every read is guarded (`except OSError: continue`): one dangling symlink
   or unreadable doc is no longer fatal.
4. Both enumerations carry a 30 s timeout, so they catch
   `subprocess.TimeoutExpired` too — it is NOT an `OSError`, so a hung git
   would have crashed the scan the enumerations were added to protect (PR
   review finding; the check-ignore call's own timeout is left as it was).

## Measured behaviour deltas

- Album ignored set 86 → 276: +240 vendored/`.venv`/`node_modules` entries
  (previously pruned by `_SKIP_DIRS`; suppression-only, a link into a
  vendored README is intentionally absent) and −50 nested-checkout paths plus
  one referenced only from vendored READMEs (neither can produce a finding:
  the core never parses those docs, and the nested checkout's paths exist on
  disk so existence short-circuits the link check).
- The corrected (repo-wide) harvest returns the SAME set as the roots-only
  revision in both real repos (houses 690, album 276): the scope bug was
  unobservable there, which is exactly why it needed a test rather than a
  two-repo run.
- Album, whole gate, old vs new orchestrator: 462 actions → 462, **0
  removed, 0 added**, identical `config_ignored` census, identical
  suppression census.
- Docs semantics on a synthetic repo that exercises both decisions
  (ignored+present orphan, ignored+absent link target): 2 docs findings with
  no list, 0 with the old list, 0 with the new list.
- houses now runs at all (58 actions, 21 record-shape, none on
  `to_dict`/`to_json`).
- Record-shape exemption unchanged by this work: album 311 → 131
  config-ignored record-shape under the current config; houses production
  112 → 29.

## Tests

- `test_gitignored_docs_harvests_every_non_ignored_doc` — a reference from a
  non-ignored doc outside the scan roots still counts (fails against the
  roots-only revision).
- `test_gitignored_docs_ignores_an_ignored_docs_own_refs` — an ignored doc is
  never enumerated, so its references need no entry.
- `test_gitignored_docs_survives_missing_git` /
  `..._survives_missing_check_ignore` / `..._survives_a_hung_git` — the three
  degradations (the last one is the `TimeoutExpired` guard).
- `test_gitignored_docs_resolves_sibling_refs_against_the_file` — kept,
  now routing the enumeration call.
- Four of the eight fail against the pre-change implementation (verified by
  restoring 5c893cb's `lucidlint.py`): missing-git, the dangling symlink,
  the nested checkout, the ignored doc's own refs. The harvest-scope test
  fails against the roots-only revision (ae74189), and the hung-git test
  against this PR's first push (no `TimeoutExpired` catch).

# Usefulness round — 0.7.0 on houses and family-history-album

The first round after the two new rules (`flat-folder`,
`sibling-value-class`) and the repo-wide root fix. Two detached worktrees at
each repo's main, the report at `--warn`, a neutral proposer per repo and an
independent design critic per repo, then a re-run of one repo after the
fixes. Artifacts lived in `/tmp/opencode/lucidlint-eval` (not committed).

## What the reports looked like

| repo | reported | fails | warnings | comment-suppressed |
|---|---|---|---|---|
| houses | 11 | 4 | 7 | 147 |
| album | 478 | 399 | 79 | 53 |

Album's 399 fails are dominated by one family: 357 `global-state` sites
(calibration constants, prompts, type aliases). The report was really one
opinion repeated, plus a minority of varied findings.

## Signals

- **Action rate.** Houses: the proposer acted on 4 of 11 and explicitly
  said that in four cases where the tool prescribed the change itself
  (split the class, type the dict as the class, add pyfakefs, merge the
  duplicate) following it would have made the codebase worse.
- **CHANGE-DIFFERS** (the message's reading right, its prescription wrong):
  - `sibling-value-class` on `Sub`/`Mul` — two operators of one family.
    Merging them is one class with a flag; the shared base is the fix.
  - `duplicate` on `_fetch`/`api_fetch` — a closure always resembles the
    function that defines it; "copy-paste" is arithmetically false.
  - `fakefs` on a test whose subject is the repo's checked-in workflow YAML
    — the rule's carve-out list did not name that case.
  - `flat-folder` on `docs` — "group the modules" when the folder holds no
    modules.
- **Critic verdicts.** The critics endorsed the proposers' code changes and
  overturned no KEEP. Both said the report is at its best as an index of
  where to look and at its worst as an instruction.
- **CONFUSING, verbatim highlights:** the config-ignored census summed
  short (599 of 627); `Lifecycle: last touched 1792686177` (raw epoch);
  "This function changed often" on a class finding; "past the 20-file mark"
  at exactly 20; a `--params <names>` placeholder in a printed command;
  "extend the ignore scope" offered for a finding in a different file.

## Shipped from the round

- `sibling-value-class`: pairs sharing a non-framework base are exempt; the
  message offers the shared-base decision before the merge; the accessor
  reader now sees f-strings and list/tuple literals (it missed both, so
  real accessors were invisible and the rule under-fired on real code).
- `global-state`: type aliases are exempt (a union/subscript alias is a
  name for a type); the message gives the decision — state to its owning
  class, a constant stays with its meaning, duplicated vocabulary gets one
  home — instead of "find the class this value serves".
- `duplicate`: same-file span containment is skipped (a nested definition
  is not a copy).
- `flat-folder`: the action matches the folder's contents, the threshold is
  stated, and the message says to leave a deliberate single-family folder
  and record why, never to invent a `misc/` or numbered bucket.
- Report: the config-ignored detail reconciles with its total (`+N more`),
  the lifecycle note renders an ISO date, the facts line no longer guesses
  "function", the config-ignored advice no longer invites widening an
  ignore, the gate names "failing targets", and every report states the
  tool version (and commit when known).

## Re-run after the fixes

Houses, same worktree: **11 → 9 reported**. The `sibling-value-class` false
positive and the nested `duplicate` are gone. A fresh proposer acted on 5 of
9, moved no directory, and used the new flat-folder wording to record the
five verdicts instead of inventing folders. Album: 478 → 460 (18 type
aliases exempted), with the census and lifecycle defects fixed.

## Deferred, with the evidence

- **`global-state` on constants stays a fail (339 sites on album).** The
  rule's recorded ruling is "values are a class's private internals"; the
  round shows the ruling is expensive on a measurement-heavy codebase. That
  is a design decision for the rule's owner, not a message fix — the
  evidence is the 339-site wall and the proposer's keep list.
- **`duplicate-module` reports structural identity as a fork.** Two
  one-field DTO modules with different fields report 100% similar and "a
  fix lands in one copy"; there is no behaviour to fork. Filed as an issue.
- **`[JUDGEMENT]` next to a runnable fix command.** The bracket says judge
  it; the line prints an executable command. Worth deciding whether a
  JUDGEMENT finding should carry a directive at all.
- **Churn's unit is unstated** (`[churn 8x]` — commits? window?). One
  clause in the header would make the number checkable.
- **`stale-suppression`'s autofix** is reported by a consumer as stripping
  the `#` from the comment that follows. Not reproduced here; needs a
  fixture before any change.

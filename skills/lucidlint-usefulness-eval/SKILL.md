---
name: lucidlint-usefulness-eval
description: |
  Reproduce the two-repo usefulness evaluation: scan real repos at main in
  detached worktrees, have a neutral agent propose changes from the report,
  have a class-design expert critique those proposals, and read the results
  as a regression test of lucidlint message usefulness.
---

# lucidlint-usefulness-eval

## Goal

Measure whether lucidlint messages drive right action on a real repo. Scan
two real repos, get a neutral agent's change proposals, get a design
expert's critique, and read the deference signal: did the proposer weigh
each finding against the code, or defer to the repo's own conventions
because the message gave them nothing to weigh?

## Inputs

- This repo (lucidlint), with a freshly built scan binary.
- Two adjacent code repos with a main branch. The round used
  `family-history-album` (origin `ashbywinch/the-loft`) and `houses`.
- A scratch directory for worktrees and artifacts:
  `${TMPDIR:-/tmp}/lucidlint-eval`.

## Prerequisites

Build the release binary after the last scanner source change:

    cd scanner && cargo build --release

A stale binary makes every scan fail with "the scan binary is required".

## Steps

Set `SCRATCH=${TMPDIR:-/tmp}/lucidlint-eval` and `mkdir -p "$SCRATCH"`.

1. **Set up the worktrees.** For each repo, fetch its main and check it out
   detached:

       git -C <repo> fetch origin main
       git -C <repo> worktree add --detach "$SCRATCH/<name>-main" FETCH_HEAD

   Use the main tip only; an unmerged branch review would not match the
   round.

2. **Scan.** From this repo:

       NO_COLOR=1 python3 lucidlint.py --repo "$SCRATCH/<name>-main" --warn \
         > "$SCRATCH/<name>-report.txt" 2> "$SCRATCH/<name>-report.err"

   Capture the rc in a file. The gate exits 1 when fail actions exist; the
   report is the input either way.

3. **Run the proposer.** Spawn one agent per repo with these instructions,
   verbatim, and nothing else added:

   > Read the file <report>. It is the output of a static-analysis tool run
   > against the repository at <worktree>. You are read-only: do not modify,
   > create, or delete any file; run no git command and no formatter.
   >
   > For each item in the report, decide whether you would make the change
   > the tool suggests, grounded in the actual code at the cited file:line.
   > Read the cited code when you need to judge. If you would act, describe
   > the concrete change: the shape of the new code, the names, the
   > structure. If you would not, say so and give your reason.
   >
   > Elaborate on anything you find confusing: any wording, metric,
   > priority, suggested fix, or expectation you do not understand. Quote
   > it, say what you think it means, and say where you lose it.
   >
   > Output: CHANGE <file>:<line> (<kind>): what you would do and why.
   > KEEP <file>:<line> (<kind>): why you would leave it. CONFUSING: a
   > bullet list. End with one paragraph on how much of the report you
   > would act on.
   >
   > Acceptance: every distinct finding or group is in CHANGE or KEEP (fold
   > near-identical items, naming each file:line); no files modified.

   Never add hints. Do not tell the agent the project's design principles,
   the evaluation's purpose, or that this is a test. The point is what the
   reader does with the message alone.

4. **Run the critic.** Spawn a fresh agent per repo, never the proposer:

   > You are an expert at good class design, coherent object models, and
   > readable maintainable code. Critique the proposal in <proposal file>
   > against the code in <worktree>: for each CHANGE item give STRONG,
   > GOOD, WEAK, or REJECT, with 1-3 sentences. Flag any KEEP you would
   > overturn. End with: is this proposal's shape the shape a careful
   > designer would want — what would you add or cut? Read the code at the
   > cited locations to ground every judgment. Read-only: make no edits.

5. **Harvest the signals.** From the two proposals and critiques, record:
   - the action rate: items the proposer would change, over items reported;
   - the verdict tally: STRONG / GOOD / WEAK / REJECT per repo;
   - the CONFUSING list, verbatim: it names report defects (rendering,
     metrics, wording) the author never sees;
   - the deference signal: whether any critic overturned a proposer's KEEP
     or questioned a repo convention the proposer deferred to. A proposer
     keeping sixty record-shape sites because "the repo documents it",
     with the critic agreeing, is the signal that the message's reasoning
     did not reach the reader.

6. **Save the artifacts** beside the reports: `<name>-proposal.md` and
   `<name>-critique.md` in `$SCRATCH`.

## Regression acceptance

Re-run the round after any change to a message, a report renderer, or the
catalog. A message change passes when the proposer re-weights the
contested family: a record-shape rewrite must make the houses proposer
argue the boundary convention instead of citing it. Read the trend, not a
fixed pass/fail: the round is a qualitative regression test.

## Failures to watch

- New items appear in the proposer's CONFUSING list: a report defect
  regressed.
- A critic overturns a KEEP: the proposer over-deferred.
- The action rate collapses: the messages became less actionable.
- A repo standard is quoted verbatim as the reason to keep: the message's
  mechanism did not land.

## Constraints

- Worktrees are detached; never commit to them.
- Every agent is read-only; state it in every prompt.
- The proposer never receives the evaluation's purpose, the class-design
  framing, or the repo's standards documents.
- The critic is always a different agent instance than the proposer.
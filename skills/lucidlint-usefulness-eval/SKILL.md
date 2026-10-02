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
   > structure. If you would make a change but not exactly the one the tool
   > proposes, write CHANGE-DIFFERS: the tool's fix, what you would do
   > instead, and why. If you would not make a change, say so and give
   > your reason.
   >
   > Elaborate on anything you find confusing: any wording, metric,
   > priority, suggested fix, or expectation you do not understand. Quote
   > it, say what you think it means, and say where you lose it.
   >
   > Output: CHANGE <file>:<line> (<kind>): what you would do and why.
   > CHANGE-DIFFERS <file>:<line> (<kind>): the tool's fix, your fix, and
   > why yours differs. KEEP <file>:<line> (<kind>): why you would leave
   > it. CONFUSING: a bullet list. End with one paragraph on how much of
   > the report you would act on.
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
   - the differs count: CHANGE-DIFFERS items per family — a message whose
     prescribed action the agent replaced is a message defect, not a repo
     defect;
   - the verdict tally: STRONG / GOOD / WEAK / REJECT per repo;
   - the CONFUSING list, verbatim: it names report defects (rendering,
     metrics, wording) the author never sees;
   - the deference signal: whether any critic overturned a proposer's KEEP
     or questioned a repo convention the proposer deferred to.

6. **Save the artifacts** beside the reports: `<name>-proposal.md` and
   `<name>-critique.md` in `$SCRATCH`.

## Reading the signals

Each signal explains itself and names the change it triggers. A careful
designer's verdicts are the ground truth; the proposer's behaviour is the
measure; the report text is what you fix.

| Signal | What it means | Change it triggers |
|---|---|---|
| CHANGE-DIFFERS: the proposer became the tool's fix for something else | The message's mechanism and action disagree: the reading is right, the prescription is wrong. In the round, duplicate-block's "delete the second copy" was right that the blocks are duplicated and wrong that deleting fixes it — the twins were load-bearing branches | Rework that family's message: write the decision rule (paste vs twin) instead of the single action |
| CONFUSING list grows | The report text misleads its reader: an entry names an artifact the reader cannot parse — an empty seam list, a lost octal, an unreconciled count | Fix the renderer or header artifact that the entry names |
| A critic overturns a proposer's KEEP | The proposer over-deferred: code the expert would change was kept as "the repo documents it" | The family's message lacked a reason the reader could weigh; strengthen the mechanism (what the code leaves unstated) |
| Action rate collapses | Proposals shrink across the board, not in one family | The last message or renderer change regressed; a message became vaguer or longer |
| A repo standard quoted verbatim as the reason to keep | The reader had nothing to weigh the finding against; the round's record-shape failure, where sixty boundary sites were kept on the repo's word | The message must state why the shape is wrong so the reader can judge whether their own standard is the wrong one |

## Regression acceptance

Re-run the round after any change to a message, a report renderer, or the
catalog. A message change passes when the proposer re-weights the
contested family: a record-shape rewrite must make the houses proposer
argue the boundary convention instead of citing it. A renderer change
passes when the CONFUSING artifacts it fixed stop appearing. Read the
trend, not a fixed pass/fail: the round is a qualitative regression
test.

## Constraints

- Worktrees are detached; never commit to them.
- Every agent is read-only; state it in every prompt.
- The proposer never receives the evaluation's purpose, the class-design
  framing, or the repo's standards documents.
- The critic is always a different agent instance than the proposer.
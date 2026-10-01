# Proposals from the #26–#34 subtask evaluation

What the evaluation was: the new tool scanned two real repos at their main
branches (the-loft, houses). A neutral agent turned each report into a change
proposal; a class-design expert critiqued each proposal against the code.
This file lists what the round shows the tool should change, in build order.
Verdict words: STRONG/GOOD/WEAK/REJECT rate a proposed change; KEEP means the
proposer left the code alone.

## What the round found

- Proposers acted on ~8% of loft's actions and ~30 houses sites. Critics
  marked 15 proposals STRONG, 48 GOOD, 8 WEAK, 1 REJECT. Every STRONG was
  verified against the code: dead classes, byte-identical duplicate
  definitions, an unreachable guard.
- No critic overturned a proposer's KEEP. Readers deferred to each repo's
  own documented conventions instead of weighing the tool's claim. The
  reason: fail messages state verdicts, not mechanisms. A reader cannot
  check "serialisation is no excuse", so the local standard wins.
- The reports' CONFUSING feedback names concrete render defects: an empty
  seam list, a hotspot that contradicts its own definition, octal `0o600`
  reported as 384, `(self)` as a loop target.

## A. Message clarity — mechanism, action, no jargon

The message rules stand in this section; the writing standard's "Clarity"
section (docs/writing-documentation.md) covers documentation prose. Every
fail message names what breaks, then what to do, in plain words.

### A1. record-shape, parameter and return types

Current:

> `data` is typed as dict but it is the serialised form of a record — and the
> erasure is the finding: every call site can build the shape ad hoc, and
> every field change happens unchecked, so the shape drifts away from the
> class. ... serialisation is no excuse for skipping the class.

Defects: "erasure" and "finding" have no meaning in the sentence; the fix
reads as "invent a class" where the class already exists in the module.

Replace:

> `data` is a dict that holds the fields of an existing class (the wire form
> its `from_dict` reads). Left as dict, call sites build the shape ad hoc and
> field changes go unchecked. Type the parameter with the class.

Three sentences: what it is, what breaks, what to do.

### A2. record-shape, dict literals

Current: "dict with constant keys {...} is a record — make a class named with
a domain noun". Defect: no mechanism; reads as a class mandate for any dict.

Replace:

> Constant keys mean this dict is a record: the keys get rebuilt at every
> build site, and each copy can drift apart. Make a class whose fields are
> the keys. If the values are objects keyed by name — a dispatch table — it
> is not a record; suppress with a why.

The dispatch escape is the rule's own: a name-keyed table of objects is not
data in a fixed shape.

### A3. data-clump

Current: "4 functions near here sit in data clumps: the parameter pairs ...
— split out a class per clump, each named with a domain noun". Defect: no
mechanism.

Replace:

> These functions each take the same parameter pair — the pair is one type
> that travels together. Threaded individually, a signature change in the
> pair propagates to every caller, and the pair has no name for shared work
> to hang on. Make one class per pair, named with a domain noun.

### A4. global-state — no configuration exemption

Current: "module-level variable 'X' — values are a class's private internals;
the variable belongs as that class's attribute/member". The earlier draft of
this proposal added an escape ("if the value is genuine configuration ...
suppress"); it is withdrawn. Configuration is state and has an owning class.
In the-loft, `IMAGE_EXTENSIONS` is the set of suffixes the adoption flow
treats as images; the owner is the workflow that ingests them. A module
global binds at import time, is read by every function, and cannot be varied
per caller — the mechanism the message should name.

Replace (keep the rule's own exempt list: dunder metadata, call-valued
registrations):

> A module-level variable is a global read: callers cannot inject or vary it
> without monkeypatching the module, and it binds at import time. Find the
> class this value serves and make it that class's attribute.

### A5. Rewrite constraints

- The `— fix:` directive tail stays; prose changes only.
- Each replacement ships test-first: a fixture whose expected message
  asserts the new text (TDD rule).

## B. Report mechanics — render defects

Fix these first; all are small.

| # | Defect (evidence) | Fix |
|---|---|---|
| B1 | Seam list empty: "these 14 findings share ONE seam (the seams: )" | Render the seam kind (file, function) when member names are empty; never an empty paren |
| B2 | Hotspot contradicts its definition: "top P99 tools/adopt.py" vs "top 10% by churn with CC>=15" | Make the named hotspot satisfy the printed definition, or print the actual rule |
| B3 | Old config read as none: repo `lucidlint.json` "actions" lists give "+0 acknowledged, no baseline" | Read old-format entries as acknowledged, or print a one-line migration note |
| B4 | Accounting unreconciled: "could not map the 64 to the printed list" | Header states acknowledged / config-ignored / comment-suppressed totals so the per-kind count reconstitutes |
| B5 | Per-item remedy contradicts the header: "suppress with:" vs "Fix findings instead of suppressing" | When a family is config-ignored, the per-item line says so and names the config key |
| B6 | bulk-suppression mislocated: repo-wide count surfaced at an arbitrary file (layout.py:1) | Render at report level, not at a file site |
| B7 | loop-pipeline renders `(self)` / `state ()` as the target | Render the actual member name or drop the parenthetical |
| B8 | `os.chmod(db, 0o600)` reported as magic number 384 | Octal/hex/binary literals are intent-bearing: exempt. Also: never fire on the value inside a named constant's own definition; skip tuple indices and length guards; honour the >=3 same-kind data-table exemption (4x12 was flagged despite it) |

## C. Precision gates — the message overpromises

### C1. duplicate-block

Current instruction "[MECHANICAL] delete the second copy" is wrong where the
twins are load-bearing branches. Evidence: rightmove_scraper:682 was a true
paste (in-function, unreachable); layout.py:856 and pipeline.py:87 were twin
branches — deletion breaks the code.

Fixes: gate the finding to blocks within the same function; replace the
instruction:

> If one block is a paste copy of the other, delete it. If these are
> intentional twins (parallel branches), the duplication is still the finding
> — extract the shared part. A twin that can diverge is a fork; a twin that
> cannot is a paste.

### C2. duplicate-module

The fork prose is false when the pair shares a base class. Evidence:
houses schools.py — "identical constants {child_age:num, stage:str}" while
the values are 4 vs 12, and the classes inherit one base.

Replace for base-class pairs:

> Two subclasses of one base with identical shape — constants are the only
> difference. If they are config-variants, the split is correct; if they were
> meant to differ, one is missing its override.

## D. The evaluation as a regression test

The round is repeatable: two worktrees, the reports, the propose-then-critique
pipeline. After each slice lands, re-run it on houses and check whether the
proposer re-weights the contested family instead of deferring to the repo
standard. Acceptance for a message change: an agent reading only the message
can name the mechanism and the action. Turn the session's harness commands
into a script when the first slice lands.

## Order of work

1. B1–B8: render and precision fixes, each with a fixture.
2. A1–A4: message rewrites; fixture assertions updated; directive tails kept.
3. C1–C2: gating changes plus fixtures.
4. Re-run the evaluation on houses; iterate on any family still deferred.
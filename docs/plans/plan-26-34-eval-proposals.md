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

The message rules stand in this section and in the message standard
(docs/writing-messages.md); the writing standard's "Clarity rules" section
(docs/writing-documentation.md) covers documentation prose. Every fail
message names what breaks, then what to do, in plain words.

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

> This dict has constant keys; the keys are fields of one record. Each build
> site re-creates the keys, so the copies can drift apart. Make a class with
> these fields and build it once. If the values select behavior (handlers,
> nodes), keep the dict as a lookup table and suppress with a why.

The exclusion separates the cases by a test only the excluded case passes:
values that select behavior are a lookup, values that are fields are a
record.

### A3. data-clump

Current: "4 functions near here sit in data clumps: the parameter pairs ...
— split out a class per clump, each named with a domain noun". Defect: no
mechanism.

Replace:

> These functions pass the same pair, (folder, fingerprint), together. The pair
> is one thing in the domain, and passing it as two parameters never says what
> that thing is. Make a class whose name states that thing, and pass one
> instance.

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

### A5. magic-number

Current: "magic number 5 — a bare literal in a computation; replace it with
a named constant". Defect: the remedy points at module scope, where a
module-level name is itself a global-state finding.

Replace:

> `5` appears bare in `walk_time`; its meaning is not stated where it is
> used. Give it a name on the class that owns this computation, and write
> that name in its place.

### A6. Rewrite constraints

- The `— fix:` directive tail stays; prose changes only.
- Each replacement ships test-first: a fixture whose expected message
  asserts the new text (TDD rule).

### A7. Full message audit

Verdicts over the complete emitted message set (57 families), against the
Message Standard. Families whose messages already state a verifiable reason
and end with the action need no change; the audit names the exceptions.

Complies as-is (42): assembly-class, boolean-arg, broad-except,
builtin-shadow, closure-cluster, conditional-polymorphism, debug-artifact,
delegating-husk, detached-method, docs-undiscoverable, duplicate,
duplicate-def, duplicate-field, fakefs, feature-envy, forwarding-chain,
god-class, guard-clauses, inline-import, latent-visitor, middle-man,
misplaced-method, monkeypatch, noqa, noop-statement, over-abstraction,
positional-literals, private-import, process-class, restating-docstring,
skipif, special-case, static-husk, strewing, swallow, tuple-record,
type-ignore, undeclared-attribute, unreachable, unused-setter, vague-name,
wide-tuple.

Needs rewrite (10 families):

| Family | Change |
|---|---|
| long-param-list | "{n} parameters — the call site cannot see what each value means, and the group has no name. Introduce one parameter object named with a domain noun." |
| partition | Append the action: "Split it into {count} classes." |
| class-module | "A class file is found by the class's name; a mismatched name hides the class from readers and importers. Rename the file to {}.py (exception: closely related models)." |
| no-assert-test | "It can never fail, so it proves nothing. Add an assertion or delete the test." |
| docs-link | "A link that leads nowhere misleads the reader. Fix the target or remove the link." |
| record-shape (return) | "f returns a dict that holds the fields of a record; the shape has no name at the call site. Type the return with the class that models the shape; create that class if none exists." |
| record-shape (param) | A1 text |
| record-shape (dict literal) | A2 text |
| data-clump | A3 text |
| global-state | A4 text |
| duplicate-block | C1 text |
| duplicate-module | C2 text |
| magic-number | A5 text |

Report-level, fixed in section B (not message rewrites): loop-pipeline
renders "(self)" or an empty state name (B7); bulk-suppression points at an
arbitrary file (B6).

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
> — extract the shared part. If a twin can diverge, it is a fork; if it
> cannot, it is a paste.

### C2. duplicate-module

The fork prose is false when the pair shares a base class. Evidence:
houses schools.py — "identical constants {child_age:num, stage:str}" while
the values are 4 vs 12, and the classes inherit one base.

Replace for base-class pairs:

> Two subclasses of one base have identical shape; only their constants
> differ. If they are config-variants, move the constants into the base and
> delete the duplicate subclasses. If they were meant to differ, one is
> missing its override.

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
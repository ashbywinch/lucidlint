# Plan — message precision round 2 (section I residuals)

Implements docs/plans/plan-message-precision-round.md section I — the
seven residuals the round-7 acceptance recorded. Every change ships
test-first (fakes only, no monkeypatch); the eval run on houses is the
acceptance instrument.

## Verified facts (grounding)

- "its value is a fixed-shape record" still renders 34 times on houses
  for dicts whose values are named types — dict[str, Provenance]
  (attempt.py:424), dict[str, Decimal] (domain.py:384) among them. The
  collection message rendered on none of them: the classifier required an
  in-module class and the return arm bypassed the check.
- Markers one line above a def — the codebase's convention — sit one
  line outside the parameter-anchored window on multi-line signatures;
  ~25 sanctioned wire-boundary sites re-report every round.
- derived_node:618 renders "express it as a timedelta
  (timedelta(minutes=86400))": the duration suggestion names a unit
  whose result is an implausible duration.
- The "highest change-cost" line carries no reason to read it as
  change-cost; round-7 reads RISK99 beside a rejected finding as
  urgency.
- A repo's acknowledged baseline entries key to old lines; the debt
  re-reports every run and the report gives no signal that the keys no
  longer match.
- loop-pipeline still fires on loops that mutate a pre-existing
  collection (settings_payload.py:209 builds nothing new).
- static-husk's fix text is open-ended for DI default adapters whose
  state lives in the module singletons.

## Phase 1 — the collection message reaches every named-type map

Actions:

1. One classifier for the dict value element, built on the AST binding
   table, never capitalization: the name resolves to a class defined in
   the module, an imported name (`from decimal import Decimal`, `from
   houses.model import Provenance`), or a known typing/builtin
   constructor — the value is a named type and the collection message
   renders in every arm: parameter, return, and dict literal. An
   unresolvable element keeps the G1 path (scalar subscripts silent;
   bare dict keeps the existing text). The {record} name in the message
   is the resolved symbol; unresolvable names render "whose values are
   named types".
2. The collection message: "{param} is a map whose values are {record}.
   The map itself has no name and no role: call sites pass a bare dict,
   and nothing says what the collection means. Make a class for the
   collection, named with a domain noun, and pass that."
3. Fixtures: `def f() -> dict[str, Decimal]`, `def g(x: dict[str,
   Provenance])`, a dict literal with class-valued elements: all render
   the collection message; none render "fixed-shape record".

Acceptance: on houses, the collection message appears at the 34 sites
and "fixed-shape record" never appears with a named-type value.

## Phase 2 — signature findings bind the def-above marker

Actions:

1. A signature finding (parameter or return anchor) binds a marker on
   the line above the def or on the def line itself — the convention's
   own size, not the 3-line constant — OR in the parameter-line window.
   A marker one line above the def binds whatever line the parameter
   anchors on.
2. Markers in the gap keep the no-over-bind rule: a marker claimed by a
   finding on its own line wins.
3. Fixtures: a multi-line signature with the marker on def-1 binds; the
   marker on def binds; the marker on the parameter line binds; a
   marker inside the body 2 lines below the def does not bind unless a
   finding sits there.

Acceptance: the ~25 sanctioned wire-boundary sites cease re-reporting;
round-8 proposer reports no def-above marker gap.

## Phase 3 — the duration suggestion states its unit only when the unit is certain

Actions:

1. The suggestion never names a unit. No unit source is certain:
   comments rot, and name-derived tokens have misfired three times on
   one value. The duration form is "express it as a timedelta in its
   unit"; the physical form is "express it as a pint Quantity in its
   unit". The family split (duration versus physical) comes from the
   token classification; nothing more. This supersedes H3's certainty
   bar and the F2 explicit-unit clause.
2. The explicit-unit examples are removed from the messages; no
   timedelta(days=1), no Quantity(5, 'kilometer'), and never
   timedelta(minutes=86400).
3. Fixtures: a `# one day` line renders "timedelta in its unit"; a
   `# 5 km/h` line renders "pint Quantity in its unit"; the
   unit-example forms appear in no emitted message.
4. The constant-RHS exemption for unit-named targets is not relied on
   here; whether it should exist is a separate, unopened question.

Acceptance: round-8 proposer finds no implausible-duration suggestion.

## Phase 4 — the change-cost line carries its meaning

Actions:

1. The percentile, the "highest change-cost" label, and the legend
   leave the report; the composite was the misleading part. The raw
   metrics stay, rendered as facts: a finding with CC >= 15, or >= 10
   changes in the history, or >= 10 callers gets one extra line naming
   exactly the facts it meets — "This function is complex (17 decision
   points) and changed often (14 changes)" — with a clause per fact,
   never a percentile, never "risk".
2. Fixtures: the CC-17 high-churn finding renders the facts line; a
   CC-4 never-changed finding renders none; no report line contains
   "risk", "RISK", or "change-cost".

Acceptance: round-8 proposer does not read the line as urgency.

## Phase 5 — baseline line drift is reported

Actions:

1. Exact-identity matching, the pyrefly pattern: an acknowledged key
   that matches no current finding is stale — drifted, fixed, and
   deleted are the same case; no reconciliation is attempted. When the
   count is nonzero the ledger renders: "N acknowledged entries match
   no current finding — re-acknowledge with --update-baseline". The
   update command regenerates the baseline, as it does for pyrefly and
   mypy-baseline.
2. Fixtures: a baseline key that no current finding occupies re-reports
   its finding and renders the clause; the same key at the current line
   acknowledges and renders no clause; a key for a fixed finding
   renders the same clause, undistinguished.

Acceptance: round-8 proposer sees why acknowledged debt re-reports.

## Phase 6 — the loop gate excludes mutation of existing collections

Actions:

1. The loop gate's body-shape check cannot tell a build from a
   mutation — it sees the write, not the target's provenance. Add the
   binding check: the target collection must have no binding before the
   loop in the enclosing scope — not a parameter, not a name bound
   earlier in the function, not a module name bound before the function
   — except a target created empty (`= {}`, `= []`) in the statement
   immediately before the loop, which is still this loop's own build.
   Anything else emits nothing.
2. Fixtures: the settings_payload shape (a parameter or earlier-bound
   dict mutated in the loop) emits nothing; the eval_context pure-build
   shape still fires; the empty-created-immediately-before shape still
   fires.

Acceptance: round-8 proposer reports no mutation-loop misfire.

## Phase 7 — the conversion states the hidden half-step

The forwarding husk is not a separate case: the state belongs in the
class, and a module singleton is itself a global-state finding. But the
remedy has a half-step the message never states. A class attribute on a
staticmethod class does not clear the finding — the rule fires on
"no self.X AND every member a @staticmethod", so the class must ALSO
stop being static: a constructor that takes the state, instance
attributes, instance methods that read them. The static-husk message
gains the explicit sentence: the methods stop being static and read the
state from the instance. Without it, a reader keeps the statics and
produces a class that still fires — the trap behind the reviewers'
resistance.

## J. Round-8 acceptance (2026-10-02) — cleared and residual

Cleared by the round-2 implementation (394 scanner + 240 pytest green,
self-check at zero, coverage 86%): the collection message resolved
Decimal and Provenance at most sites; the def-above marker binds; no
unit is named in any suggestion; the mutation-of-existing loop gate
silenced the settings_payload shapes; the static-to-instance sentence
is in the husk message; the ledger is additive, the change-cost line and
legend are gone, and the stale-acknowledged clause is test-verified
(renders with --baseline).

Residual and new:

1. Collection-message coverage is inconsistent: attempt.py:424
   (dict[str, Provenance]) still renders "its value is a fixed-shape
   record" while attempt.py:626 renders the collection message — same
   shape, two arms, two messages. The resolution must cover every site
   of a named-type map, in every arm, and the round-8 proposer asks a
   rule question the message must answer: when a typed, documented map
   is kept, what does "Make a class for the collection" ask that the
   annotation has not already given?
2. The duration family claim misfires on conversion factors: bus.py:192
   and commute_router.py:663 divide by 60, and the message says the 60
   is a duration. The family inference needs the same certainty bar the
   unit got — or the family sentence drops and the general message's
   "write it as a named constant" is the whole action.
3. Async loops (property_nodes:828,830; park_and_ride_augment_node:227)
   carry the loop-pipeline tag and its "Replace Loop with Pipeline"
   recipe although no async comprehension exists; the tag suppresses
   for async loops or names asyncio.gather.
4. The report never states which anchor a record-shape variant uses
   (def, parameter, return literal, nested literal); round-8 spent a
   CONFUSING bullet on exactly that, alongside the body-first-line
   placement that still misses def-anchored findings with no
   explanation.
5. Cross-site magic values: domain.py:119 is exempt as unit-named while
   transit.py:161 flags the same default; no view links the two, so one
   drifting value reads as two decisions.
6. Duplicate-module asymmetry: http_error's near-duplicate pair goes
   unflagged while schools.py:140 (a 6-line constant difference) fires;
   the similarity thresholds need the pair's two sides named.
7. Process-class and latent-class boilerplate: unclear what clears the
   bar; api_router.py:530 and capture_dom.py:394 are misdescribed; a
   defensive accessor-copy carve-out is asked for registered_nodes.
8. P5's stale clause is unit-tested; the acceptance run passed no
   --baseline, so the clause's live rendering awaits a baselined run.

## Order of work

1. Phase 1, then Phase 6 — the two premise/goal defects.
2. Phases 2 and 3 — the window and the unit.
3. Phases 4, 5, 7 — the render and message texts.
4. Rebuild the release binary after the last scanner edit; full
   battery (cargo test, pytest, make check, self-check, coverage);
   eval round 8 on houses; section I items absent from its CONFUSING
   list.
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

1. A duration suggestion names a concrete unit only when the unit is
   explicit in the comment ("# one day"); name-derived tokens alone are
   never certain enough for the example.
2. Otherwise the action is "express it as a timedelta in its unit".
3. Fixtures: `# one day` comment renders timedelta(days=1); the
   name-only 86400 renders "in its unit"; timedelta(minutes=86400)
   never renders.

Acceptance: round-8 proposer finds no implausible-duration suggestion.

## Phase 4 — the change-cost line carries its meaning

Actions:

1. The "highest change-cost" line appends the reason: "— the code
   most expensive to change, not the most important finding".
2. Fixture: the line renders the appended clause.

Acceptance: round-8 proposer does not read the line as urgency.

## Phase 5 — baseline line drift is reported

Actions:

1. When the repo's baseline entries acknowledge no finding on the
   current lines (the file's acknowledged keys match nothing scanned at
   their lines), the ledger appends: "N acknowledged entries point at
   lines that no longer hold the finding — re-acknowledge with
   --update-baseline".
2. Fixture: a repo with a baseline whose keys are stale renders the
   clause.

Acceptance: round-8 proposer sees why acknowledged debt re-reports.

## Phase 6 — the loop gate excludes mutation of existing collections

Actions:

1. loop-pipeline emits only when the loop builds a NEW collection; a
   loop that mutates a pre-existing dict or list emits nothing.
2. Fixtures: the settings_payload shape (mutating an existing dict)
   emits nothing; the eval_context pure-build shape still fires.

Acceptance: round-8 proposer reports no mutation-loop misfire.

## Phase 7 — static-husk's fix text is scoped

Actions:

1. When the husk's members forward to module-level singletons or DI
   defaults, the message names that case and the honest action
   (document the seam) instead of the open-ended "find the state"
   instruction.
2. Fixture: a DI default adapter renders the scoped text.

Acceptance: round-8 proposer reads the static-husk guidance without the
DI-default confusion.

## Order of work

1. Phase 1, then Phase 6 — the two premise/goal defects.
2. Phases 2 and 3 — the window and the unit.
3. Phases 4, 5, 7 — the render and message texts.
4. Rebuild the release binary after the last scanner edit; full
   battery (cargo test, pytest, make check, self-check, coverage);
   eval round 8 on houses; section I items absent from its CONFUSING
   list.
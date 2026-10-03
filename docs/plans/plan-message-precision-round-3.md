# Plan — message precision round 3 (section J residuals)

Implements docs/plans/plan-message-precision-round-2.md section J — the
round-8 residuals. Every change ships test-first (fakes only, no
monkeypatch); the eval run on houses is the acceptance instrument.

## Verified facts (grounding)

- attempt.py:424 (dict[str, Provenance]) renders "its value is a
  fixed-shape record" while attempt.py:626 renders the collection
  message: the same shape, two arms, two messages (round-8).
- bus.py:192 and commute_router.py:663 divide by 60; the message calls
  the 60 a duration.
- Async loops (property_nodes:828,830; park_and_ride_augment_node:227)
  carry "Replace Loop with Pipeline" although no async comprehension
  exists.
- Round-8 spent a CONFUSING bullet on the anchors: a record-shape
  finding can anchor at the def, the parameter, or a nested literal,
  and the report never says which; a marker on the body's first line
  misses def-anchored findings with no explanation.
- domain.py:119's 30 is exempt as a unit-named named field while
  transit.py:161's same default is flagged — one drifting value, two
  decisions, no cross-reference.
- dag/http_error.py and houses/http_error.py are near-duplicate
  modules the reader attests, while schools.py:140 (a 6-line constant
  difference, shared base) fires.
- The latent-class closures message at api_router.py:530 says "write
  to 1 captured local (accumulator)" where the closures only read; the
  process-class bar and the defensive accessor-copy case draw
  CONFUSING entries.
- P5's stale-acknowledged clause is unit-tested; the acceptance run
  passed no --baseline, so the live rendering is unverified.

## Phase 1 — the collection message is consistent and the alias case is answered

Actions:

1. The collection classifier fires in every arm for every resolvable
   named-type map; the attempt.py:424 shape is a fixture and must
   render the collection message.
2. A raw-dict alias (`Sources = dict[str, Provenance]`) is NOT an
   exemption: an alias names the shape, it carries no behavior. The
   message names the difference and the move: "Make a class for the
   collection, named with a domain noun — an alias to dict names the
   problem, it does not solve it. Move the operations that take, build,
   or read this map onto the class as methods, splitting them where the
   class does not own all the work; which operations belong there is
   the judgement call." The operations threaded around the map are the
   class's latent member functions; the split is the reader's call.

Acceptance: every named-type map on houses renders the collection
message with the member-function instruction; round-9 proposer reports
no two-message inconsistency and no "alias suffices" reading.

## Phase 2 — both mechanisms named, the family left to the reader

Actions:

1. The family assertion leaves the message; the mechanisms stay. The
   magic message names both alternatives and never claims which
   applies: "Write it as a named constant where the computation uses
   it, and state what it means and why this size in the name or in one
   comment beside it. If the value carries units, express it in a type
   that understands them — Python's timedelta for durations, or a
   Quantity from the pint units library for physical measures." The
   principle is general; the two are glossed examples; the family is
   the reader's judgment.
2. Fixtures: the 60-division site renders the general text plus both
   mechanism sentences, never "is a duration"; the retry-cap site
   renders the same; no unit is named for either.

Acceptance: no round-9 CONFUSING item about durations versus
conversion factors.

## Phase 3 — async loops carry no pipeline tag

Actions:

1. A loop inside an async function emits no loop-pipeline finding.
2. Fixture: an async `while`/`for` with awaits emits nothing; the sync
   pure-build shape still fires.

Acceptance: round-9 proposer names no async pipeline recipe.

## Phase 4 — the anchors are stated in the guidance

Actions:

1. The suppression guidance sentence names the anchors and the window
   positions exactly: a record-shape finding anchors at the def, the
   parameter, or the literal it names; a marker binds within the 3
   lines ending at the anchor, on the line above the def, or on its
   own line. A marker on the body's first line binds nothing unless a
   finding sits there; the guidance says so, and the report adds no
   new finding and no marker advice.

Acceptance: round-9 proposer's marker questions are answered by the
guidance sentence, not by discovery.

## Phase 5 — a same-value name elsewhere is linked

Actions:

1. When a flagged literal's value matches a name bound in the same
   module (a named constant or an exempt unit-named field), the
   message appends: "the same value is named at {file}:{line}".
2. Fixture: a flagged 30 with a named 30 elsewhere in the module
   renders the cross-reference; no name anywhere renders nothing.

Acceptance: round-9 proposer sees the domain.py:119 / transit.py:161
pair as one value, not two decisions.

## Phase 6 — duplicate-module compares function skeletons too

Actions:

1. The duplicate-module comparison adds function-level skeleton
   similarity (the body_skeleton machinery already in place), so a
   pair of modules sharing one function's skeleton forked apart is
   paired even when their constants differ. The schools:140 shared-
   base case keeps its config-variant message.
2. Fixture: the http_error pair (dag/ and houses/) fires duplicate-
   module; schools:140 keeps the base-class text.

Acceptance: round-9 proposer sees the http_error pair flagged and no
COMФUSING asymmetry entry.

## Phase 7 — the steering messages state what the detector sees

Actions:

1. The latent-class closures message states only what is true: the
   nested functions close over the enclosing locals. The accumulator
   phrase renders only when the closures mutate a captured local.
2. The process-class message states the clearing bar in decision
   terms: keep the class when the domain genuinely names the stateful
   component; otherwise the operations belong on the objects they
   operate on.
3. record-shape does not fire on a return that is an annotated
   accessor copy of the class's own state (return self.X).

Acceptance: round-9 proposer reports neither misdescription.

## Phase 8 — the stale-acknowledged clause on a baselined run

Actions:

1. The round-9 evaluation runs with `--baseline lucidlint.json`
   passed, exercising the stale clause live.

Acceptance: the clause renders with its count when drift exists and
stays absent when every acknowledged key matches.

## Order of work

1. Phase 1, then Phase 6 — the consistency and completeness defects.
2. Phases 2 and 3 — the wrong claims.
3. Phases 4, 5, 7 — the guidance and message texts.
4. Phase 8 — the verified run; rebuild the release binary after the
   last scanner edit; full battery; eval round 9 on houses with
   --baseline passed; section J items absent from its CONFUSING list.
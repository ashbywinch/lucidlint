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

## Phase 4 — a marker binds on the finding's line or the line before

Actions:

1. The binding window is exactly two lines: the finding's own line and
   the line immediately before it. This supersedes the 3-line window,
   the def-window union (round 2, Phase 2), and the def-1/def variant:
   all of them admitted markers that were not on the finding's line or
   the line before, which no reader regards as related.
2. The anchors stay on the problem lines — the parameter's line for a
   parameter's type, the return annotation, the literal the finding
   names — so the practiced "one line above the def" placement binds
   single-line signatures (finding on the def line) and is one line
   high only where the problem is an interior parameter line, which the
   reader can see and fix in one line.
3. The guidance sentence states the rule in one sentence; the existing
   3-line tests are rewritten to the two-line rule.

Acceptance: round-9's marker CONFUSING entries are absent; no marker
binds from a distance greater than one line.

### Phase 4a — the logical line is also an anchor

One line of code can span several source lines: a multi-line def, a
wrapped call, a split literal. A marker binds when it sits on or one
line above the finding's own source line, OR on or one line above the
logical line containing that code — its first source line (the def
line for a signature, the statement's first line for a wrapped call).
Statement-level precision survives: a marker on the specific physical
line still wins, innermost first.

Implementation: the emitter records each finding's logical start line
in an internal field marked `#[serde(skip)]` (JSON stays schema 4); the
binder's window becomes {logical_start-1, logical_start} ∪ {line-1,
line}. Fixtures: a marker above a multi-line def binds every parameter
and return finding in that signature; a marker above a wrapped call's
first line binds the finding inside it; a marker on the physical line
binds; two lines above either anchor binds nothing. The guidance
sentence states the logical-line rule.

## Phase 5 — removed

Value equality is not relatedness. 30, 60, and 100 recur across
unrelated code, and the detector knows only the number; a cross-
reference would invite wrong merges — two unrelated constants made one
— and asserts a relationship the tool cannot know. The round-8
observation was mild (one site exempt, one flagged, no wrong action),
and the asymmetry is correct: the exempt site's name states the value's
role, the flagged site's does not.

## Phase 6 — duplicate-module compares function skeletons too

Actions:

1. The similarity measure stands alone: compatible entity kinds plus
   Dice >= 0.9 over the skeleton bigrams. The constants leave the gate
   entirely — no identity requirement, no non-empty requirement, no
   member-name requirement. The http_error pair (dag/, houses/) has no
   module-level constants at all, which is what silenced it; it fires
   once the gate is the measure.
2. The message stops claiming constants by default: the "with
   identical constants {…}" clause renders only when the constants
   exist and match; the matched members stay as reader information.
3. Two subclasses of one base keep the config-variant message (the
   detector sees the shared base, and the fork prose is false there).
4. Fixtures: the http_error pair fires; the schools config-variant
   pair keeps its message; a pair below 0.9 stays silent. Any
   lookalike-but-unrelated pair the widened gate surfaces is brought
   to the round-9 acceptance rather than pre-filtered away.

Acceptance: round-9 proposer sees the http_error pair flagged and no
COMФUSING asymmetry entry.

## Phase 7 — the steering messages state what the detector sees

Actions:

1. The latent-class closures message states only what is true: the
   nested functions close over the enclosing locals. The accumulator
   phrase renders only when the closures mutate a captured local.
2. The process-class message states the three-part test that clears
   it: "{Class} is named for a process, not a thing. Keep the name only
   when all three hold: 1) the domain calls this component by that
   name; 2) the data it operates on is all already represented by
   well-designed classes; 3) data and logic still remain that
   legitimately fit better in a process class. If the data it operates
   on is still raw dicts, lists, or loose fields, represent it with
   classes first, then re-check 2 and 3." The detector states the part
   it can see; the reader runs the test.
3. Removed — no accessor-copy exemption. A class's collection-typed
   data should itself be a class, with its instance owned by the larger
   class. A plain list held or returned by a class is an unmodelled
   collection, not data that is already owned, so the finding stands at
   those sites.

Acceptance: round-9 proposer reports neither misdescription.

## Phase 8 — the stale-acknowledged clause on a baselined run

Actions:

1. The round-9 evaluation runs with `--baseline lucidlint.json`
   passed, exercising the stale clause live.

Acceptance: the clause renders with its count when drift exists and
stays absent when every acknowledged key matches.

## K. Round-9 acceptance (2026-10-02) — cleared and residual

Cleared by the round-3 implementation (395 scanner + 240 pytest green,
self-check at zero, coverage 86%): the collection message renders
identically at attempt.py:424 and :626, and the round-9 reader quotes its
alias line back ("an alias to dict names the problem, it does not solve
it"); both mechanisms render with no family claim (bus.py:192 no longer
"is a duration"); async loops emit nothing; the closure message carries
no false accumulator claim; the process-class three-part test is in the
message; duplicate-module pairs constant-free module forks (the
http_error pair fires, as modules) while its constants clause renders
only when they exist and match; and the baselined run exercised the
stale clause live — 9 acknowledged, "86 acknowledged entries match no
current finding — re-acknowledge with --update-baseline" — which the
round-9 reader acted on exactly as intended.

Residual:

1. The two-line window is implemented as settled, and the round-9 reader
   reports ~20 markers placed five or six lines above their findings that
   no longer bind, with nothing explaining it. The rule is right (own
   line or the line before); the repo's marker placement must move, or
   the markers are documentation the tool does not read. No tool change
   proposed until the user rules.
2. The duplicate-module refinement (class pairs require matching
   constants; modules pair on shape alone) cleared seven lookalike class
   pairs in this repo; the http_error pair fires as a module pair. Two
   class pairs remain possible on other repos; watch round 10.
3. The houses config-ignore for global-state grew 37 to 333 sites,
   against the repo's own "no blanket config ignores" decision; the
   round-9 reader asks that the ignore be re-scoped and the rest report.
4. The houses baseline needs re-anchoring (86 drifted keys); re-running
   --update-baseline after reviewing the 9 live entries is the reader's
   stated plan.
5. 55 broad-except sites: the reader proposes one shared boundary-catch
   helper in the repo so the per-site markers disappear; a repo change,
   not a tool change.

### Phase 4a result (measured)

The logical-line anchor landed: the marker window is the finding's own
line, the line before it, the logical line's first source line, or the
line before that. Houses fails fell from 187 to 139 with the same
baseline — 48 signatures the repo had marked above their defs bind
again. The internal logical-start line is `#[serde(skip)]`, so the JSON
stays schema 4. 401 scanner tests. Two markers in scanner/src/main.rs
went stale (no record-shape fires in that file) and were deleted; the
guidance sentence states the logical-line rule.

## Order of work

1. Phase 1, then Phase 6 — the consistency and completeness defects.
2. Phases 2 and 3 — the wrong claims.
3. Phases 4 and 7 — the guidance and message texts.
4. Phase 8 — the verified run; rebuild the release binary after the
   last scanner edit; full battery; eval round 9 on houses with
   --baseline passed; section J items absent from its CONFUSING list.
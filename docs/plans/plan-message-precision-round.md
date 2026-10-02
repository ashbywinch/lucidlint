# Plan — message precision round (record-shape premises, suppression window, loop gating)

Implements section E of docs/plans/plan-26-34-eval-proposals.md (round-3
acceptance defects) plus the round-2/3 artifacts the evaluations kept
flagging. Every behavior change ships test-first (fakes only, no
monkeypatch); the eval run on houses is the acceptance instrument.

## Verified facts (grounding)

- Round 3 (2026-10-02, houses at 12f416f, new binary): the proposer
  argued the record-shape premise instead of citing the repo convention —
  the acceptance passed — and its argument named premises that are false
  at specific sub-shapes (plan E, items 1-7).
- record-shape: 74 fail sites on houses; the wire-parse sub-family builds
  no dict at the site (from_dict consumes it); the scalar-value sub-family
  (derived_node.py:882 `dep_timestamps`, property_nodes.py:659
  `name_by_id`) holds no record at all; the internal-ad-hoc sub-family
  matches the A1 text.
- magic-number: fires on `Quantity(20, 'minute')` inside a field named
  `max_walk_to_station` — the unit is stated at the site; the message's
  "meaning is not stated where it is used" is false there
  (houses/settings.py:69-71, houses/model/domain.py:119).
- Suppression window: a marker on the SAME line as its except does not
  bind (houses/server.py:704, confirmed by round-2 and round-3
  proposers); a marker one line past the 3-line window is reported
  "stale — remove it" although it documents the literals it sits above
  (tools/commute/rightmove_url.py:57,60). Placement misses and genuine
  staleness are distinguishable: the family fires nearby (mis-placed) vs
  nothing fires in the file (stale).
- loop-pipeline: 204 warnings on houses, 202 kept; the template is issued
  for a PriorityQueue drain, a polling sleep-schedule ("fold: interval =
  max/sum/min(...)"), and a polyline decoder — the shape does not fit the
  recipe. The pure single-accumulator builds (eval_context.py:42) are the
  ~2% that do.
- Header: "95 acknowledged action(s)" sits in the repo lucidlint.json but
  the GATE line still reads "+0 acknowledged" until --baseline is passed
  (round-3 CONFUSING).
- class-module: "suppress with: class-module" vs "fix: split-module" —
  the suppression kind and the fix kind differ, so the two actions in one
  message never line up (round-3 CONFUSING).
- [RISKxx] per-item tags still read as brokenness order even after the B2
  rename; the top-risk line carries the real information
  (round-3 CONFUSING: "the only true complexity fail carries the LOWEST
  risk display value").

## Phase 1 — record-shape premises hold at every firing sub-shape

Point: one message cannot serve three sub-shapes with two contradictory
premises ("call sites build it ad hoc" is false at parse edges, and
"fixed-shape record" is false for scalar maps). Classify at detection,
then emit the message whose premise is true.

Actions:

1. Classify each constant-key dict site into one of three sub-families:
   - internal-ad-hoc: the dict is built at one or more NON-parse sites in
     the module (assignments, function returns, calls);
   - wire-parse: the dict arrives as a parameter or return of a function
     named from_dict/from_json (or of any function whose result feeds
     `from_dict`) and no site builds it ad hoc;
   - scalar-map: every value in the literal is a scalar (str/int/float/
     bool/None) — a lookup keyed by identity, not a record.
2. Detection:
   - scalar-map: do not emit record-shape.
   - wire-parse: emit with the wire message: "The dict is the wire form
     of a record; the parse boundary has no type. Give the shape a named
     record and ingest the wire with its from_dict — call sites can then
     type against the record instead of the wire."
   - internal-ad-hoc: emit the current A1/A2 text (the premise holds).
3. Each sub-family gets a fixture with its own expected message; the
   scalar-map fixture asserts NO record-shape finding.
4. docs/writing-messages.md gains the authoring rule the round proved:
   a mechanism clause must hold at every sub-shape the family fires on —
   when a family has sub-shapes with different mechanisms, the message
   branches at emission.

Acceptance: `cargo test` green; on the houses scan, scalar-map sites no
longer fire, wire-parse sites carry the wire message, ad-hoc sites carry
the ad-hoc message.

## Phase 2 — magic-number: unit-stated literals are named

Point: "its meaning is not stated where it is used" is false when the
unit rides in the same expression.

Actions:

1. Exempt a literal whose parent Call names a unit constructor
   (Quantity, timedelta, TimeInterval, and any call whose keyword carries
   a unit name: `Quantity(x, "minute")`, `timedelta(minutes=20)`), and a
   literal in a field whose name states the unit.
2. Fixtures: `Quantity(20, "minute")` on `max_walk_to_station` does not
   fire; `return a * 60` still fires with the A5 text.
3. Keep the B8 exemptions; extend the exemption test table.

Acceptance: `cargo test` green; the round-3 CONFUSING entry for
settings.py:69-71 / domain.py:119 disappears on the next houses scan.

## Phase 3 — suppression window: same-line binds; mis-placed is not stale

Point: trailing same-line markers exist (server.py:704) and must bind;
a marker one line past the window is documentation, not staleness, and
the advice must not say "remove it".

Actions:

1. Extend the marker window to include a trailing comment on the
   finding's own line.
2. Split the stale-suppression verdict by evidence:
   - the signal family fires near the marker but outside the window →
     mis-placed: advice is "move the marker into the 3 lines ending at
     the finding", never "remove";
   - nothing fires in the file for that family → stale: "remove it".
3. Fixtures: the server:704 shape (marker on the except line) binds and
   clears; the rightmove_url:57 shape reports mis-placed with the move
   advice.
4. The suppression guidance sentence in the render states: a marker
   binds on the finding's own line or within the 3 lines ending at it.

Acceptance: `cargo test` + `pytest` green; round-4 proposer's CONFUSING
has no same-line or one-line-past window entry.

## Phase 4 — the line-up items (each small, one fixture)

1. class-module: when the fix kind differs from the signal kind, the
   per-item line links them: "suppress with: class-module (this family's
   fix is split-module)". Render-side, test in test_lucidlint.py.
2. GATE line: when a repo-root lucidlint.json holds acknowledged actions
   and --baseline was not passed, append "(N in lucidlint.json, not
   activated)" to the "+0 acknowledged" phrase.
3. Per-item tags: drop [RISKxx] from finding lines; keep the single
   top-risk line in the header. Update the tag test.

Acceptance: `pytest` green; round-4 proposer reads the class-module and
RISK lines without CONFUSING entries.

## Phase 5 — loop-pipeline: emit only where the shape fits

Point: 202 of 204 warnings are the template issued for shapes the recipe
cannot address; the finding should exist only when a comprehension is
actually applicable.

Actions:

1. Gate the emission on the loop shape: emit loop-pipeline only for a
   loop whose body builds one collection via pure per-item computation
   (no early exit, no side effect, no second accumulator). Drain loops,
   sleep-schedules, and state machines emit nothing.
2. For the kept shapes, drop the fold/hoist recipe sentences entirely —
   no finding, by construction.
3. Fixtures: eval_context.py:42 shape still fires; a PriorityQueue drain
   and a polling sleep-schedule do not.

Acceptance: `cargo test` green; the houses loop-pipeline warning count
drops toward the 204-sites' pure-build subset, and the round-4 proposer
reports no loop-pipeline CONFUSING entries.

## Order of work

1. Phase 1 (record-shape classification; the largest single change).
2. Phase 3 (window) — independent of 1, small; then Phase 2.
3. Phase 5 (loop gating).
4. Phase 4 (line-up items).
5. Rebuild the release binary after the last scanner edit; full battery
   (cargo test, pytest, make check, self-check, coverage); eval round 4
   on houses; the plan's section E items 1-6 must be absent from the
   round-4 CONFUSING list.
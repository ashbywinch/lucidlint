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
   literal in a field whose name states the unit — the unit is already
   stated, so the value is named.
2. When a bare literal's surroundings state units — the enclosing names
   carry them (`distance_km`, `max_walk_to_station`), a unit comment sits
   on the literal, or the expression mixes incompatible-looking values —
   the message actively suggests the unit mechanism, not just a name:
   "This is a quantity, not a bare number: express it as a pint Quantity
   (`Quantity(5, 'km/hour')`) so the unit rides with the value and
   conversions are checked; name it where the computation owns it." When
   the file does not import pint, the message adds that pint is the
   house units library and names introducing it.
3. Fixtures: `Quantity(20, "minute")` on `max_walk_to_station` does not
   fire; `5` beside a `# 5 km/h` comment fires with the pint-suggestion
   text; `return a * 60` still fires with the A5 text.
4. Keep the B8 exemptions; extend the exemption test table.

Acceptance: `cargo test` green; the round-3 CONFUSING entry for
settings.py:69-71 / domain.py:119 disappears on the next houses scan.

## Phase 3 — suppression window: same-line binds; mis-placed is not stale

Point: trailing same-line markers exist (server.py:704) and must bind;
a marker one line past the window is documentation, not staleness, and
the advice must not say "remove it".

Actions:

1. Extend the marker window to include a trailing comment on the
   finding's own line. The server:704 shape (marker on the except's
   line) binds and clears.
2. Markers outside the window do not bind, and the report says nothing
   about them: the finding fires, the marker stays as documentation.
   There is no advice to move a marker and no mis-placed verdict — the
   tool never tells the reader to relocate a comment.
3. "Stale — remove it" is reserved for the one case it means: the
   family fires nowhere in the file. The rightmove_url:57 shape (marker
   one line past the window, family firing below) produces no stale
   verdict and no advice.
4. The suppression guidance sentence in the render states: a marker
   binds on the finding's own line or within the 3 lines ending at it.

Acceptance: `cargo test` + `pytest` green; round-4 proposer's CONFUSING
has no same-line or one-line-past window entry.

## Phase 4 — report lines that mislead (each small, one fixture)

1. class-module names: the suppression advice names the signal
   (class-module) and the fix names the fixer (split-module) — two words
   for one family. Link them in the line: "suppress with: class-module
   (this family's fix: split-module)". Reader-side test.
2. Headline truth: when a repo-root lucidlint.json holds acknowledged
   actions and --baseline was not passed, the "+0 acknowledged" phrase
   appends the flag itself — "(N acknowledged in lucidlint.json — pass
   --baseline lucidlint.json to activate them)" — so the reader learns
   the flag from the message.
3. [RISKxx] per-item tags: read as priority, they are actually the
   churn x fan-in percentile; the valuable fail (complexity) shows the
   lowest number and wire noise the highest. Drop the tag from finding
   lines; keep severity and the header's single top-risk line. Update
   the tag test.

Acceptance: `pytest` green; round-4 proposer reads the class-module
line and the risk line without CONFUSING entries.

## Phase 5 — loop-pipeline: emit only where the shape fits

Point: loop-pipeline is a warning that says "this loop builds a
collection — replace it with a comprehension (or fold it)". The template
is correct only for a loop whose body is one pure per-item computation
building one collection (`for k, v in items: result[k] = f(v)` becomes
`{k: f(v) for k, v in items}`). The detector fires the same template at
loops with no collection to build: a priority-queue drain (pop, process,
requeue), a polling loop that sleeps and checks, a polyline bit-decoder
that mutates state per bit, a counter loop. "fold: interval =
max/sum/min(...)" is nonsense for those shapes. On houses, 202 of the
204 warnings are that misfire, and the CONFUSING lists keep naming it.

Actions:

1. Emit the finding only when the loop genuinely reduces to a
   comprehension: a pure per-item computation building one collection,
   with no early exit, no side effect, and no second accumulator.
2. Every other loop emits nothing — no finding, by construction.
3. Fixtures: the eval_context.py:42 shape (pure dict build) still fires;
   a PriorityQueue drain and a polling sleep-schedule do not.

Acceptance: `cargo test` green; the houses loop-pipeline warning count
drops toward the 204-sites' pure-build subset, and the round-4 proposer
reports no loop-pipeline CONFUSING entries.

## F. Round-4 acceptance (2026-10-02) — PASS; five residual defects

The precision round landed (366 scanner + 230 pytest green, self-check at
0, coverage 86%). The eval re-ran on houses: loop-pipeline warnings went
204 to 15 with zero recipe complaints; per-item RISK tags, the move-a-marker
advice, and the invalid baseline headline are gone; the scalar-map
taxonomy reached the reader (property_nodes:665 kept as "dict of scalars,
not a record"). The proposer now reuses existing record classes instead of
creating ones (bus.py empty-case dicts, commute_breakdown — and caught a
latent str/float type bug doing it), and the from_dict family is kept with
an argument, not a citation.

Residual defects, for the next iteration:

1. Wire message at record-exists sites: ~30 from_dict sites already HAVE
   the named record and the ingestion; the wire text "give the shape a
   named record and ingest the wire with its from_dict" describes a step
   already taken. Detect ingestion-exists (a class in scope with
   from_dict for the same shape) and do not emit at those sites — only
   wire sites without a record get the message.
2. Pint suggestion fabricates units: Quantity(86400, 'minute') is
   dimensionally wrong at derived_node:618 and bus.py:192. Emit the unit
   clause only when the unit is unambiguous; otherwise "express it as a
   pint Quantity (its unit)" without a fabricated unit.
3. Window anchoring: markers one line above a def do not bind although
   the guidance says "within the 3 lines ending at the finding" — the
   def-line anchor and the finding line must be reconciled, and the
   guidance must match the implementation exactly.
4. Stale message without the rule delta: markers whose findings are now
   exempt (unit-named values) read "nothing fires in this file" with no
   explanation; the stale message should state the kind and note the
   current exemptions when the file's values are covered by them.
5. Top-risk headline: the single top-risk line presents a from_dict site
   as the report's headline while the valuable fixes sit elsewhere. The
   line ranks what a change would cost; it does not rank which code is
   wrong. Fixing the most expensive code first is a coherent strategy
   when the goal is to lower future change cost. Fix the presentation,
   not the information: label the line "highest change-cost: the most
   expensive code to modify safely", and decide whether the code is
   wrong by reading the finding's message, not the risk line. Re-weight
   the metric so complexity and size are not dominated by the churn
   factor — the round evidence (a CC-17 knot at RISK01 while wire noise
   heads the report) is the calibration data.

## Order of work

1. Phase 1 (record-shape classification; the largest single change).
2. Phase 3 (window) — independent of 1, small; then Phase 2.
3. Phase 5 (loop gating).
4. Phase 4 (line-up items).
5. Rebuild the release binary after the last scanner edit; full battery
   (cargo test, pytest, make check, self-check, coverage); eval round 4
   on houses; the plan's section E items 1-6 must be absent from the
   round-4 CONFUSING list.
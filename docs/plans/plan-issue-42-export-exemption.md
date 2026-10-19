# Plan — issue #42: the record-shape export exemption (to_dict/to_json)

Fixes github.com/ashbywinch/lucidlint issue #42. Every change ships
test-first (fixtures + Rust unit tests); the eval run on houses is the
acceptance instrument.

## Verified facts (grounding)

- The repro (`@dataclass` `Report` with `to_dict -> dict` building
  `dict(rid=…, fresh=…)`) reports twice on 0.6.7 (commit `5c893cbc`):
  the return arm at the annotation (checks.rs:7638 pre-change) and the
  literal arm at the `dict(...)` call.
- Eight variants measured before the change (each file scanned alone):

  | variant | pre |
  |---|---|
  | dataclass fields + `to_dict -> dict`, `dict()` call form | 2 |
  | same, no return annotation | 1 (literal) |
  | same, `-> dict[str, object]` | 2 |
  | same, unannotated `self.x` in `__init__` | 2 |
  | `to_dict` adding a key that is not a field | 2 |
  | `to_dict` on a class declaring no field | 2 |
  | non-export name (`snapshot`) on a field-bearing class | 2 |
  | module-level `def to_dict()` | 2 |

- The ingestion mirror already exists (F1, checks.rs `module_has_from_dict_class`):
  a wire-named function in a module carrying a `from_dict` class emits
  nothing, because the message's advice is a step already taken. The
  export side had no mirror.
- The rule's own boundary ruling (fixture
  `record_deserializer_boundary_is_still_a_finding__01.py`) names the
  recommended shape as "a class with named fields AND a serialisation
  method at the edge" — flagging that method's return contradicts it.
- The return arm's advice is unsatisfiable on the canonical form: the
  shape's class exists and is the enclosing class, so "create that class
  if none exists" asks for a redundant second type (the sweep the issue
  reports added TypedDict twins beside ~85 record classes).

## The rule (chosen: name-gated, option A)

An EXPORT EDGE is a `to_dict`/`to_json` method whose enclosing class
declares fields — annotated in the class body, annotated `self.<x>` in
`__init__`, or `self.<x>` assigned in a method. Name-gated to mirror the
ingestion side's `from_dict`/`from_json` pair; `as_dict` is deliberately
NOT included (add it only if the houses eval leaves leftovers).

At an export edge:

1. the return-annotation arm does not fire — the class names the shape
   the method returns;
2. the literal arm does not fire for a literal inside the method whose
   constant keys are covered by that class's declared fields (a
   projection of the class's own fields is still the class's shape).

Everything else is unchanged: a `to_dict` on a class that declares no
field, a non-export-named method, a module-level `to_dict`, and a
literal adding a key the class does not declare all keep their findings.

Implementation: `ExportEdge` + `module_export_edges` (checks.rs, beside
the F1 helpers) precompute the module's edges once — `name_start` keys
the signature arm, `start_line`/`end_line` + `fields` filter each
literal. No walk restructuring, no message rewording.

## Phases

### Phase 1 — the exemption (done)

1. Fixtures: `record_export_to_dict_class_fields_passes__01.py`
   (dataclass annotated, unannotated `__init__` fields, `dict()` and
   literal forms, `-> dict` and `-> dict[str, object]`, `to_json`),
   `record_export_to_dict_uncovered_key_fires__01.py` (one extra
   non-field key → the literal arm alone, line pinned),
   `record_export_non_edge_to_dict_keeps_findings__01.py` (field-less
   class, non-export name, module-level `to_dict` → 6 findings).
2. Tests beside the F1 pair in scanner/src/main.rs; the two exemption
   tests fail before the change, the non-edge test pins unchanged
   behavior.

### Phase 2 — acceptance (done)

1. The eight variants re-run through the CLI: the four canonical forms
   report 0 record-shape findings (2/1/2/2 before), the uncovered-key
   form reports the literal arm alone at the literal's line, and the
   three non-edge forms report 2 each — the table above, post-column
   measured.
2. `make self-check` green (docs integrity included), `make test` green
   (404 Rust tests + 241 Python tests), `cargo clippy --all-targets --
   -D warnings` and `cargo fmt --check` clean.
3. The fix directive goes with the finding: on the repro,
   `lucidlint fix --kind extract-record-class` leaves the file
   untouched, while the uncovered-key variant still extracts.
4. One existing fixture moved off the now-exempt case:
   `record_suppression_one_line_above_def_binds__01.py` used a
   `to_dict` return finding to pin the marker-above-def window; that
   finding no longer exists, so the method is now `snapshot` and the
   test keeps pinning the window (the marker would otherwise report
   stale, correctly).

## Residual false negatives (accepted)

- An export-named method on a field-bearing class returning an OPAQUE
  different shape (`return build_payload(self)`) loses the return arm
  and has no literal to catch it; the construction site's own literal
  still fires in practice.
- A `Base.to_dict` reaching a subclass's field keeps the literal arm
  (under-exemption, the conservative direction).

## Open question (not taken)

Option B: drop the name gate and exempt ANY method whose literal's keys
are covered by its enclosing class's fields. Wider false-negative
surface, no name-based arbitrariness, and it would also clear a
`snapshot()`-style method returning the class's own fields. Take it only
if the houses eval still reports such methods as noise.

# Plan — issue #57: data-clump groups by name, not type

Fixes github.com/ashbywinch/lucidlint issue #57. Every change ships
test-first (fixtures + Rust unit tests); the repro is the acceptance
instrument.

## Verified facts (grounding)

- `data_clump_findings` (`scanner/src/checks.rs`) collected only
  `parameter.name`; annotations were never read. Three functions taking
  `(first, second)` therefore clumped even when the types were `Wall`,
  `Point`, and `tuple[int, int]` (the issue's repro).
- The consequence is not only a noisy message: the `extract-class` fix
  preview on the repro generated one `WallPair` class from the pair
  names and rewrote all three signatures to take it. `distance` and
  `ranges_touch` do not share `Wall`'s fields, so the generated type is
  false.
- `record-shape` already treats a fixed tuple as a record
  (`annotation_is_record`), but both message arms hard-coded "dict"
  (`checks.rs`), so `pair: tuple[int, int]` and `-> tuple[int, int]`
  both claimed the annotation was a dict. `list[dict]` and
  `list[tuple[int, int]]` hit the same text.
- RULES.md's `wide-tuple` row says "2-tuples are pairs", while
  `record-shape` flags fixed 2-tuples as records. The issue accepts the
  finding if the message says "unnamed positional record"; this plan
  keeps the finding and fixes the wording. Exempting 2-tuples is a
  separate policy change.

## The rule

### data-clump: annotation agreement is disconfirming evidence

The grouping key is `(name, annotation key)`:

- annotation key = the annotation's source text, whitespace-normalized,
  resolved through module-level type aliases (`WallAlias = Wall`);
- `""` is the unannotated key and matches only other unannotated
  parameters — an absent annotation is not evidence of a shared type;
- the three-function threshold is applied per key, so a module can emit
  one clump per annotation group.

Split policy, chosen deliberately:

| case | outcome |
|---|---|
| all three unannotated | fires (current untyped behavior) |
| annotations agree, including aliases | fires |
| annotations differ | splits; no finding unless a group reaches three |
| two annotated + one unannotated | splits; no finding |
| three annotated + one unannotated | fires, naming only the three |

The fixer mirrors the same key so it selects the group the finding
names; a mismatched pair declines instead of generating a false class.

### record-shape: the message names the shape written

The signature message classifies the annotation before choosing text:

- fixed tuple → "a fixed-size tuple; its positions are an unnamed record";
- list → "a list whose items are unnamed records";
- dict → the existing fixed-shape-record text.

The parameter and return arms both use the classifier. Map-value
messages (the collection class) are unchanged.

## Phases

### Phase 1 — regression tests (done)

Fixtures: the issue repro, agreeing typed functions, alias agreement,
typed/unannotated split. Rust tests fail on the name-only scanner for
the repro and split cases.

### Phase 2 — typed grouping (done)

`param_annotation_key` + typed pair maps in `data_clump_findings`;
`_module_aliases` / `_annotation_key` / `_typed_args` in
`fix_engine._clump_at`.

### Phase 3 — shape-aware record-shape messages (done)

`SignatureRecordShape` + `signature_record_shape`; both message arms
branch on it. The fixed-tuple test now asserts the tuple text and the
list fixture asserts the list text.

### Phase 4 — verification

`cargo test`, `make check`, `make test`, `make self-check`, and the
issue repro. Expected: no `latent-class` finding; the tuple
`record-shape` finding remains with tuple wording.

## Out of scope

- `lucidlint.py`'s dedupe key `(kind, file, line, function)` collapses
  the second tuple parameter on a one-line signature; the repro reports
  one `record-shape` finding for two tuple parameters.
- `record-shape`'s catalog description describes a struct-with-fields
  arm that the scanner does not implement, so RULES.md still drifts
  from the signature/literal behavior.

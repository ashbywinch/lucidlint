# Plan — rule destinations and private code leakage

Follows the 0.8.0 message review run on `houses` (`608e54b4`, 255 findings) and
`the-loft` (`a655d813`, 681 findings). The review's through-line: several
high-volume rules name a mechanic and then ask the reader to make a decision
the detector can already make. This round fixes the detectors and the messages
together, one PR per change.

## Principle

A message names the destination when the evidence names it. It does not say
"decide which before editing", "reconsider whether this class is the correct
abstraction", or "if this is deliberate, keep both" when the detector can see
which case the code is. If the detector cannot tell, it does not emit the
finding — it never hands the reader a question the tool could answer.

This is the message standard's existing "state only what the detector knows",
applied to the action as well as the fact.

## 1. `private-import` becomes `private-code-leakage`

The rule today catches `from houses.school import _try_geopoint` and
`import houses._internal`. It misses the third way code leaks out of a module:

```python
import houses.school
houses.school._try_geopoint("51.4", "-1.0")
```

The rename states the full problem, and the rule gains the attribute arm.

- Kind: `private-import` → `private-code-leakage`; severity stays `fail`.
- Covers: a private name imported from another module, a private module path
  imported, and an underscore attribute reached on a module imported from
  another module.
- The kind is a config, suppression and baseline key. The rename updates the
  four `lucidlint: ignore private-import` sites in `houses` in a paired PR (a
  different repository cannot ride the same PR); no alias is kept (an alias
  would leave two names for one rule).
- The message says renaming is not the fix. A public name is a promise to
  every caller: widening the module's contract to silence one call site creates
  the coupling the boundary exists to prevent. The action is to call the public
  API that expresses the need, or move the behaviour into the owning module;
  making it public is a deliberate contract change, named for the capability,
  not a renamed implementation detail.
- Before widening the surface, the message asks the reader to check two things:
  the class mixes concerns and the function belongs on the class that owns them
  (where it would be public), or the caller is doing work the callee should
  own. Public is the answer only when neither holds.
- A package's `__init__.py` may reassemble names from its own package; that is
  the facade's job, not leakage. A peer module may not, and another package's
  private name is leakage even in a facade. The rule row states the exemption.
- The attribute arm found twenty facade reaches in `fix_engine/__init__.py`
  (its documented re-export surface) and two genuine reaches:
  `RustScan._actions_from_rust` and `RenderCtx._FEEDBACK_REQUEST`. The two
  become public in this round; the facade is exempt by the rule.
- Tests: the three forms fire; a public import and a public attribute do not;
  the facade reassembles its own names, a peer module reaching the same name
  does not, and a facade reaching another package's private name does not.

## 2. Private functions do not count toward `several-definitions` / `many-definitions`

A module-level function whose name starts with one underscore is a helper of
the module's subject, not a second concern. `houses/school.py` is one class and
two private parsers; it fires today and should not.

- The exemption is honest because privacy is enforced in the same tree:
  `private-code-leakage` fails the import and the attribute form. A helper that
  another module can reach is not private.
- Public functions still count. A module of unrelated public definitions still
  fires. The ladder's unit stays "definitions"; only the private helper is
  removed from the count.
- A module-level `__getattr__` / `__dir__` is a protocol hook (PEP 562), like a
  decorated framework entry point; other dunders (`__str__`, `__repr__`) are
  names nothing calls and stay counted.
- No separate `many-functions` rung: the name is the old label for this ladder,
  and a second count over the same definitions would report one module twice.
  A large private module remains covered by `large-function`, the file-edge
  measure and `flat-folder`.
- Tests: `houses/school.py`'s shape is silent; a module of three unrelated
  public functions still fires; a private helper reached from another module
  is a `private-code-leakage` failure, not a silent exemption.

## 3. A duplicate names its home when the evidence names one

Two new outcomes, decided by the detector, not the reader.

**A repeated small construction has a home.** `GeoPoint(float(lat), float(lng))`
appears at `houses/school.py:106`, `:181`,
`tools/update_school_coords.py:88`, and `:118`. It is under
`duplicate-expression`'s 15-node floor and its enclosing functions are not
`duplicate`-similar, so neither rule sees it. (`houses/apis/nominatim_api.py`
builds the same class from a subscripted JSON row; that expression is over the
floor, so `duplicate-expression` owns it.)

- New kind `shared-construction` (warn, Python): the same call shape — same
  callee, same argument conversions — in two or more functions or modules,
  where the callee is a project class and the arguments are raw values being
  converted. The message names the class and offers the constructor to add:
  "Add one named constructor on `GeoPoint` (for example `from_values`) and call
  it everywhere."
- It does not fire on a direct constructor of already-typed values
  (`GeoPoint(lat, lon)` with `float` parameters), on builtins, or on one site.

**A deliberate parallel implementation is not a duplicate.** Same-named methods
on a test double and its production store (`require`, `to_dict` in the loft), or
per-type adapters (`_with_people` / `_with_places` in `tools/archive.py`), are
parallel implementations with different types. The rule does not emit a
duplicate finding for them, and the message never says "keep both" — the
standard forbids approving duplication. When the pair is the same logic over
different types, the finding is the *type* question, not a duplicate.

- Detector: compare the parameter and return types and the literal/attribute
  differences. Same name plus different types plus a test-double/production
  pair or a per-type adapter is not a duplicate.
- When the types are structurally identical and the difference is only names,
  report one duplicate with the merged type named.

## 4. `derived-method` (warn) names the direct form

The call-shaped `misplaced-method` stays a fail. The direct form is a warning:
the value may be for one caller, so the reader decides.

A module-level function whose first parameter is a class this module defines,
which reads through it at two or more places, never uses it as a bare value,
and returns one expression with no conditional, is a method in disguise —

```python
def _format_address(school: School) -> str:
    return f"{school.name}, {school.postcode}"   # belongs on School
```

The function must read through the parameter at two or more places and never
use it as a bare value (a return, an argument, an assignment). A one-field
accessor stays: a getter is not always a method. A bare use means the function
needs the object, not the object's fields.

Negative: `_parse_gender(raw: str)` has no class parameter and stays.
The message states both outcomes: "`_format_address` takes only a `School` and
reads its fields. If every caller would want the same value, it belongs on
`School` as a method. If only this caller wants it, keep it with the caller."

## 5. `feature-envy` gains the free-function form

The method form is unchanged. The free-function form resolves the annotated
class against the repo-wide class index: a module-level function that takes
exactly one parameter, a class the repo defines, reads its fields four or more
times, and never uses the object as a bare value is that class's method in
exile. The index is what keeps a `Path` or `Request` from being named as a
home; the class is the destination, and the function name is the method.

## 6. The drift check runs in `make check`

`RULES.md` and `scanner/src/rules_gen.rs` are generated from
`rule_metadata.CATALOG`; `scripts/gen-rules.py --check` validates both
directions. It ran only in `make test` (via `tests/test_lucidlint.py`), so a
stale generated file could pass the pre-push hook. `make check` now depends on
`rules-check`, which runs the same check; the pytest assertion stays.

## 7. Audit: the declared language is not verified

`private-import` declares `implementations=("python", "rust")`, but only the
Python AST emits it. The language column is hand-declared per rule; the
inventory checks the kind, not the language. Record this as an audit item:
either verify every declared language has an emission, or derive the column
from the emission sites. This round fixes `private-code-leakage`'s declaration
to the language that actually emits it; the full audit is its own change.

## Rejected, with the recorded reason

- `several-classes` — tried; Python's unit is the class, and the Rust-count
  transplant was wrong (`plan-issue-107-module-shape.md`).
- `hub-file` — deleted 2026-11-17: fan-in measures popularity, not mixedness;
  a shared model is legitimately imported by every rule module.
- `duplicate-near` — no finding lived in the 0.7–0.9 band on either repo.
- `duplicate-module-similarity` — rejected in the #26–#34 round.
- `large-module` — the first cut, replaced by `several-definitions` /
  `many-definitions`.
- `many-functions` — not a distinct rule; the ladder counts definitions.

## Measurement and gates

Each PR records the before/after on `houses` and `the-loft`: the finding counts
by kind, the new kind's examples, and the false positives the change avoids.
`make check`, `make test` and `make self-check` are green before the PR. No
threshold is changed to make a repo pass.

## Order

1. This plan and the `AGENTS.md` routing row.
2. `private-code-leakage` (rename + attribute arm + `houses` suppressions).
3. Private-helper exemption in the definition ladder.
4. `shared-construction` and the duplicate-home message.
5. `misplaced-method` direct form and `feature-envy` free-function form.
6. `gen-rules.py --check` in `make check`.

# Plan — issue #119: a record's constructor is not behaviour

Fixes github.com/ashbywinch/lucidlint issue #119. The rule's own exemption
already said "all but one class being a value record is one concern plus its
records"; the detector counted a constructor as behaviour, so the exemption
stopped at the record that has one. Every change ships test-first.

## The report, claim by claim

| claim | verdict |
|---|---|
| `attach_impl_methods` counts every `ImplItem::Fn` as a method, so a constructor-only record is behavioural | TRUE (`scanner/src/rustscan/modshape.rs`) |
| `ModuleShape::classes_dominate` then counts that record as a second class | TRUE (`scanner/src/checks/module_size.rs`) |
| A record with a constructor is still a record | TRUE — the rule's exclusion is about the type's role (a value with a name), and a constructor builds the value |
| The three options offered (`receiver methods`, `constructor names`, `state + behaviour`) | the issue declined to choose; the third is verdict-identical to the first, and the second is a name list that misreads `Marker::signal_and_why` |

Reproduction (old core, this host): a module of `Suppressions` (receiver
methods) + `Marker` (`fn parse -> Vec<Marker>`, `fn signal_and_why -> (String,
String)`) + one free function reported `holds 3 top-level definitions, 2 of them
types (Marker, Suppressions)`. A Python module of one class with a `self` method
plus one class whose only method is `__init__` plus a free function reported the
same shape: the defect is the rule's, not Rust's.

## The rule

Behaviour is a method that acts on one of the type's values.

- Rust: an impl fn that takes `self`; a trait item fn likewise. An associated
  function builds values or judges them from outside — a record's algebra, not
  a concept that owns its state.
- Python: a method that is not `__init__`/`__new__` and is neither
  `@staticmethod` nor `@classmethod` — the language's own markers for "takes no
  instance".

`TypeShape.methods` now counts only those, so a record with a constructor, a
parser, or a `to_dict` no longer makes its module "several". A namespace — a
fieldless type whose impl is all associated functions — is a record by the same
test; the rule's remedy ("fold them into the one that owns the state") cannot
name a stateless owner, so claiming one was never checkable.

The message says what the detector now does: "A type whose impl acts on no value
of its own adds no behaviour, so it may stay beside the type that uses it."

## The side effect the corpus exposed: which arm speaks

Silencing the types arm for a record-heavy module left the functions arm as the
only voice, and it spoke for modules the functions do not dominate:
`loft/live/batch_row.py` became "holds 7 top-level definitions, 2 of them
functions … make them a class" — a message about two helpers beside five types.
The two arms were never a partition (`owned > functions` for the types arm, and
`functions >= 2` for the functions arm), so a record-heavy module fell into the
gap and was described by the wrong arm.

The arms now partition the module:

- types arm: `class_equivalents >= functions` (the class equivalents are at
  least the bulk) and a second concept of its own — more than one type behaves,
  or a receiver group is another class equivalent;
- functions arm: `functions > class_equivalents` (the functions are strictly
  the bulk) and at least `FUNCTIONS_GROUP_MIN` of them;
- neither: one class, its records, and at most as many helpers is one concept.

A tie belongs to the types, because a helper belongs to the class it is about.
The types message names the receiver groups too, so every definition in its
count is visible to the reader.

## Measured

Old core (`origin/main`, `e50592d`) and new core, both scanned through the same
orchestrator at the two corpora's `main`; `(file, kind)` diff:

| repo | before | after |
|---|---:|---:|
| family-history-album | 683 | 674 |
| houses | 229 | 220 |

Nine findings removed per repo, none added. Every removed finding is a module of
one class (or one namespace) plus its records plus its helpers — the shape the
exemption names:

| module | shape |
|---|---|
| `loft/live/document_row.py`, `page_row.py`, `review_row.py`, `batch_row.py` | one `*Projection` + its `*Row`/`*State`/`*Bundle` records + helpers |
| `loft/live/migrate.py`, `schema.py`, `pipeline/model/ai_client.py` | one class + an error/value record + helpers (`Schema` is all `@classmethod`) |
| `pipeline/api/approval.py`, `pipeline/evals/eval_review.py` | one behavioural class + its records (eval flows) + helpers |
| `houses/{apcoa_scraper,bus_journey,comments,stations}.py`, `dag/measurement.py` | one class or namespace + its records + helpers |
| `houses/{map_layers,town_desc,transit_route}.py`, `houses/model/domain.py`, `houses/web/settings_payload.py`, `tools/commute/searches.py` | records (`from_dict`/`to_dict` only) + helpers |

Three modules keep their finding and change arm, from the functions message to
the types message, because their types and functions tie: `pipeline/detect/mark.py`
(5 types), `pipeline/stage_table.py` (4), `houses/model/domain.py` (10, three of
them behavioural). The message now names the types the reader is looking for.

No removed finding had two behavioural types: the positive case (two classes that
act on their values) still fires in both languages, pinned by tests.

## Residuals

- Python dunders other than the constructors (`__repr__`, `__eq__`) still count
  as behaviour, so a value class that declares one is not exempt. Whether a
  value's dunders are "acting on its values" is a separate decision; this round
  did not widen the exemption.
- A module of several stateless namespaces is no longer judged by this rule: it
  holds no stateful owner for the message to name, and #107's `mixed-module`
  family is the signal for a module whose parts never reference each other.
- `docs/plans/plan-issue-107-module-shape.md` still says "a class with no
  methods is a value record"; that doc records the round that shipped the
  ladder. `RULES.md` and this doc are the current statement.

## Review refactor

The review found the arm predicate hard to read, not wrong. The refactor makes
the decision structural:

- `ShapeArm` (Types/Functions/None) is the one arm decision; both findings
  match on it instead of repeating the `if`/`else`.
- `class_equivalents()` names what the types arm owns (types plus receiver
  groups); `types_dominate()` and `functions_are_a_group()` name the two
  comparisons. The tie and the `None` silence are documented at the decision.
- The types message now includes the receiver-group clause, so a module whose
  count includes a function group can be checked by the reader.
- The new tests are real files under `tests/fixtures/code/`, not inline
  strings. On rebase, `code-in-literal` (from #126) reported all seven inline
  snippets; they were migrated rather than suppressed. The rule caught exactly
  the smell the PR would otherwise have reintroduced.

The one thing the tool could not have caught is the message omission: the
counts were right but one counted category was unnamed. That is a message test
gap, now pinned by `several_definitions_types_arm_names_the_receiver_groups`.

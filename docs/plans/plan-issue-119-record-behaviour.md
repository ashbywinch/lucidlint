# Plan — issue #119: a record's constructor is not behaviour

Fixes github.com/ashbywinch/lucidlint issue #119. The detector counted a
constructor as behaviour, so a record looked like a second behavioural class.
Behaviour is now a method that acts on the type's values. The threshold is
unchanged: more than two top-level definitions fires, and a Rust class
equivalent (a type plus its impl/functions) is one definition. Every change
ships test-first.

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
parser, or a `to_dict` is not behavioural. The module still fires when it holds
more than two top-level definitions; the constructor decides the behaviour
count the message states, not whether the finding exists. A namespace — a
fieldless type whose impl is all associated functions — is a record by the same
test.

## More than two definitions fires

The rule is a count. The arms only choose the message:

- types arm: `class_equivalents >= functions` (the class equivalents are at
  least the bulk) and a second concept of its own — more than one type behaves,
  or a receiver group is another class equivalent;
- functions arm: `functions > class_equivalents` (the functions are strictly
  the bulk) and at least `FUNCTIONS_GROUP_MIN` of them;
- when neither side is the bulk, the types arm names the types when there are
  any, otherwise the functions arm.

There is no third, silent arm: a record-heavy module with more than two
definitions fires. A tie names the types when there are types, because the
helpers belong to the class they are about. The types message names the receiver
groups too, so every definition in its count is visible to the reader.

## Measured

Old core (current `main`) and new core, both scanned through the same
orchestrator on the two corpora's `main`; `(file, kind)` diff:

| repo | before | after | change |
|---|---:|---:|---:|
| houses | 249 | 255 | +6 `several-definitions` |
| family-history-album | 676 | 681 | +5 `several-definitions` |

No finding is removed: the constructor change corrects the behaviour count and
the message, but these corpora's record-heavy modules still hold more than two
definitions, so they now fire where the previous arm gate was silent. The added
modules are:

- houses: `houses/commute.py`, `houses/nodes/{bus_route_node,commute_breakdown_node,property_detail,property_summary}.py`,
  `tools/check_docs_links.py`
- album: `document/transcript.py`, `loft/live/{change,compat_pages,errors}.py`,
  `research/spike-word-segmentation/render_marks_wholepage.py`

The positive case (two classes that act on their values) still fires in both
languages, pinned by tests.

## Residuals

- Python dunders other than the constructors (`__repr__`, `__eq__`) still count
  as behaviour, so a value class that declares one is not exempt. Whether a
  value's dunders are "acting on its values" is a separate decision; this round
  did not widen the exemption.
- A module of several stateless namespaces is judged by the count like any
  other module; the message names the types, and `mixed-module` remains the
  signal for parts that never reference each other.
- `docs/plans/plan-issue-107-module-shape.md` records the round that shipped
  the ladder; its record exemption sentence is superseded by this round.
  `RULES.md` and this doc are the current statement.

## Review refactor

The review found the arm predicate hard to read, not wrong. The refactor makes
the decision structural:

- `ShapeArm` (Types/Functions) is the message choice; both findings match on
  it instead of repeating the `if`/`else`. A module over the threshold always
  fires.
- `class_equivalents()` names what the types arm owns (types plus receiver
  groups); `types_dominate()` and `functions_are_a_group()` name the two
  comparisons. The fallback to types (or functions when there are no types) is
  documented at the decision.
- The types message now includes the receiver-group clause, so a module whose
  count includes a function group can be checked by the reader.
- The new tests are real files under `tests/fixtures/code/`, not inline
  strings. On rebase, `code-in-literal` (from #126) reported all seven inline
  snippets; they were migrated rather than suppressed. The rule caught exactly
  the smell the PR would otherwise have reintroduced.

The one thing the tool could not have caught is the message omission: the
counts were right but one counted category was unnamed. That is a message test
gap, now pinned by `several_definitions_types_arm_names_the_receiver_groups`.

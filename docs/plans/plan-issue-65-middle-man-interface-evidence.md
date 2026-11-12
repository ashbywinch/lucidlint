# Plan — issue #65: middle-man interface evidence

Fixes github.com/ashbywinch/lucidlint issue #65. Test-first (fixtures + Rust
unit tests); the two-repo run is the acceptance instrument.

## Verified facts (grounding)

- The rule is `middle_man_candidates` + `middle_man_findings` (checks.rs): a
  method whose body is one `return self.<chain>(...)` fires unless an
  exemption applies. #44 added three: the target is not declared in the
  scanned codebase (a library), the method is an interface member, the call
  fixes policy the target defaults.
- The interface arm read repo-wide state: member name → the parameter-name
  sets every interface in the repo declares, and a name match exempted the
  wrapper. Nothing tied the declaration to the class (reproduced: with a
  `Renderer(Protocol)` in one file, a real `Wrapper.render → _inner.render()`
  in another goes silent — 0 findings; delete the protocol file and it fires).
- The #44 fixtures put the interface in the same file as the adapting class,
  so the #44 shapes are, structurally, "the interface lives with the class".
  The issue's own repro keeps `Adapter` silent and expects `Wrapper` to fire —
  both classes are untyped and structurally identical apart from the module
  their protocol is declared in and the target member's name. Name+arity alone
  cannot separate them; neither can the issue's "member set exactly matches"
  alternative.
- One class per file is the house convention, so "the protocol is in another
  module" is the NORM for a shared `protocols.py` — the exemption has to keep
  reading an explicit relationship. `class Adapter(Renderer)` is that
  relationship; `class-module`'s #52 exemption already uses this pair ("names
  it as a base, or the declarations-only interface in its own module").

## The change

`MiddleManClass` carries its `module`. The interface index stores, per member,
`InterfaceDeclaration { interface, module, names }` — the declaration's own
identity, not a bare name set. `implements_interface_member` exempts only when
the wrapper accepts the declared parameters AND the declaration is about this
class: the interface is declared in the owner's module, or the owner names it
as a base at any distance through the repo's bases (`named_bases`).

A distant `Protocol` that the class does not name is no longer evidence. The
adapter in the issue's repro stays silent (protocol and adapter share a
module, the #44 shape); the real delegation fires.

### The shape the narrowing exposed: language-invoked operators

Narrowing the arm re-exposed four houses sites — `Measurement.__add__`,
`__sub__`, `__mul__`, `__truediv__` delegating to `self._binop` — whose only
exemption had been an unrelated `ABC` in the package. Every one is a dunder:
it is invoked by `a + b`, so no call site exists where "call the target
directly" can be taken. That is the #47 test (a finding a reader cannot act
on must not be reported), so `is_dunder` exempts the operator methods, and
the fixture pins that the exemption is the operator naming — an ordinary
method over the same helper still fires.

## Measurement (both repos at main, old vs new binary, `--warn --json`)

| repo | findings old | findings new | added | removed |
|---|---|---|---|---|
| houses | 9 | 9 | 0 | 0 |
| family-history-album | 83 | 83 | 0 | 0 |

The album's one apparent diff is `bulk-suppression`'s repo-wide census naming
a different file per run (a suppression census line, not a middle-man
finding); the finding sets are otherwise identical.

Issue repro (`case2_adapter.py` + `case4_real_middle_man.py`): old 0
middle-man findings, new 1 (`case4_real_middle_man.py:10`). The dunder
fixture: old 1 (`__add__`), new 0.

## Tests

- `middle_man_interface_evidence_lives_with_the_class_it_serves` — the
  issue's repro: the adapter sharing its module with the protocol stays
  silent, the real delegation fires (old binary: 0 findings, so this fails
  before the change).
- `middle_man_interface_named_as_a_base_exempts_across_modules` — an explicit
  `class Adapter(Renderer)` is exempt wherever the protocol is declared.
- `middle_man_skips_the_language_invoked_operators` — the dunder is silent,
  and an ordinary method delegating to the same helper still fires.
- The #44 fixtures stay green unchanged (`middle_man_exempts_the_encapsulation_shapes`,
  `middle_man_reads_the_metaclass_spelling_of_an_interface`,
  `middle_man_keeps_the_delegation_this_codebase_can_answer`).

## Residuals (not fixed here)

- **A cross-module protocol the class does not name no longer exempts.** A
  duck-typed adapter whose `protocols.py` it never inherits from now fires
  "call the target directly" — the finding's action there is "name the
  protocol (`class Adapter(Renderer)`), or remove the wrapper", which is a
  lucidity improvement, but the message does not yet say so. The message is
  the next step if the eval shows the shape on a real repo.
- **Same-named classes still merge** (the #44 limitation): a chain resolving
  to `Inner` may read a `render` declared by an unrelated `Inner`. Unchanged;
  the interface evidence is per declaration now, but the target declaration
  is still a union.
- **`__init__`-style call sites of a non-operator dunder are unaffected** —
  `is_dunder` is naming-based, and every dunder is language-invoked.

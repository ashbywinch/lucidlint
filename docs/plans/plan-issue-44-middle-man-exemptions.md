# Plan — issue #44: middle-man exemptions (library targets, interface members, fixed defaults)

Fixes github.com/ashbywinch/lucidlint issue #44. Test-first (fixtures + Rust
unit tests); the two-repo run is the acceptance instrument.

## Verified facts (grounding)

- The rule is `middle_man_findings` (checks.rs): a method whose body is one
  `return self.<chain>(...)` fires unconditionally. Its advice — "call the
  target directly" — is un-actionable in three of the issue's four shapes:
  the target is a library's method (case 1: `self.model_dump(by_alias=True)`
  on a pydantic base — inlining re-states `by_alias` at every call site, and
  one that forgets emits a different wire shape), the method is an interface
  member (case 2: callers are typed against a `Protocol`), or the call fixes
  an argument the target defaults (case 3: `self.dump(by_alias=True)` — the
  policy lives in the named method).
- Fixtures, pre → post (the pre binary is HEAD before this change):

  | fixture | pre | post |
  |---|---|---|
  | library target (`self.model_dump(by_alias=True)`) | 1 | 0 |
  | Protocol member (`render` → `self._inner.emit(format=…)`) | 1 | 0 |
  | fixed default (`self.dump(by_alias=True)`) | 1 | 0 |
  | unknown attribute (`self.inner.go(x)`, no evidence of `inner`) | 1 | 0 |
  | real delegation (`Inner.render`, no args) | 1 | 1 |
  | interface name collision, arity mismatch | 1 | 1 |
  | the original `middle_man_delegation_detected__01` shape | 1 | 1 |

- Repo measurements: houses 5 → 2, album 1 → 0. Every removal is an exempt
  shape (`compute` over a property chain, `call_impossible` over a base
  method in another module, `fare_between` over a dict, `read` over
  `str.encode`); the two kept houses sites are `_FakeRailFareRegistry`
  wrappers whose target class is in the same file and whose call adds
  nothing — the rule's own case.

## The change

`collect_middle_man` runs per file and hands the repo-wide pass two
collections: the file's classes — whether each is an interface
(`ABC`/`ABCMeta`/`Protocol` base or an `@abstractmethod` member), its methods
with their parameter shape (positional-required count, names with defaults,
every non-receiver name), its bases, and which class each `self.<attr>` is
built from (an annotated field, `self.x: T`, or `self.x = T(...)`) — and its
delegation candidates (owner class, attribute chain, the call's own shape,
the wrapper's name and parameters). `middle_man_findings` then merges every
file's classes into one index and decides.

**Repo-wide, not per file.** One class per file is the house convention, so
the target of a delegation usually lives in ANOTHER file: a file-local index
reads almost every real target as "outside the codebase" and silently drops
the finding for exactly the production wrappers the rule is for. (First cut
was per-file; the reviewer's argument that a one-class-per-file codebase
makes cross-file targets the NORM, not the exception, is what moved it.)

A single-statement `return self.<chain>(...)` then fires only when ALL hold:

1. the chain resolves to a member this codebase declares — the `self.<attr>`
   steps resolve through the merged index (the attribute's class, then that
   class's own attributes) and the member may sit on the receiver class or on
   any base of it. Same-named classes merge (union of methods/bases/attrs):
   an ambiguous name means "could be this target", which is what the
   resolution asks, and the exemptions then read the union's most forgiving
   declaration;
2. no interface the codebase declares has a member of that name whose
   parameters the wrapper accepts (a conforming implementation, not a name
   collision);
3. the call does not fix policy — no keyword the target defaults, no
   positional beyond the target's required ones (`**mapping` is forwarding,
   not policy).

`self_call_chain` replaced the old `is_self_call`: it returns the attribute
chain, so the receiver class is resolvable rather than assumed. The candidate
owns its own verdict (`policy_fixes`, `fires`, `finding`) — the tool's own
`record-shape` finding asked for that on the first cut, twice.

## Single-buffer scans (recorded, not a defect)

`--file` and the LSP's typing path never run a repo-wide pass, so they decide
against the file's own classes: a target in another file is invisible there
and the rule stays silent (the conservative direction — fewer findings, never
a wrong one). The gate and the LSP's save path use the repo-wide index.

## Known limitations (recorded from the PR review, 2026-10-21)

- **Same-named classes merge** (union of methods/bases/attrs), so a chain
  resolving to `Inner` may read a `render` declared by an unrelated `Inner`.
  Deliberate: an ambiguous name means "could be this target", and a scanner
  with no name resolution cannot pick the right one; the union's most
  forgiving declaration decides. Revisit only if the eval shows it firing on
  real code.
- **Interface conformance compares parameter NAMES, not types.** A wrapper
  whose parameter has the wrong type but the right name is exempted. The
  alternative — comparing annotation text — would fire on compatible-but-
  different spellings (`Sequence[str]` vs `list[str]`), trading a rare false
  negative for a common false positive in a tool with no type checker.
- **A nested class's attributes are not read as the enclosing class's.** The
  review flagged this as reachable; it is not: `push_stmt_children` descends
  `ClassDef` bodies but never a nested `FunctionDef` body, so a nested class's
  `__init__` was already unreachable from the enclosing class's walk (a fixture
  for it passes before and after the suggested guard, so no guard was added).
- **An attribute with no type evidence resolves to nothing.** `self._inner =
  get_inner()` is read as "outside the scanned codebase" and the rule stays
  silent: `class_attribute_classes` reads an annotation (`self.x: T`) or a
  constructor call (`self.x = T(...)`) and nothing else, and a factory's
  return type needs inference the scanner does not do. Treating the unknown as
  in-codebase would re-fire on exactly the library-target shape this exemption
  exists for. Probe: `self._inner = get_inner()` + `return
  self._inner.render()` → 0 findings; `self._inner = Inner()` → 1.
- **A chain step whose class is outside the codebase ends the resolution.**
  `declarations` returns empty and `fires` returns false — the library-target
  exemption itself: a library class's members are invisible to the scan, so no
  member of it can be judged.
- **`self.a().b()` is not a chain.** `self_call_chain` reads attribute steps
  from `self`, and the receiver here is a call's result (`base_name_of` reads
  `Name`/`Attribute` only), so there is no candidate. Deliberate: resolving it
  needs `a()`'s return type, and "call the target directly" would mean
  re-invoking `a()` at every call site — the wrapper is where that call
  belongs. Probe: `return self.build().render()` → 0 findings.
- **A wrapper class defined inside a `try` is not walked.**
  `middle_man_candidates::walk` descends `If`/`For`/`While`/`ClassDef` but not
  `Try`. The *target* inside a `try` IS read (`ModuleClasses::read` walks via
  `push_stmt_children`, which descends `Try`), so a conditionally defined
  target resolves and fires; only the conditional *wrapper* is missed, in the
  conservative direction. The review's claimed case — a delegation inside a
  `try` — is unreachable for a different reason: a candidate's body must be
  exactly one `return`, and a `try`-wrapped delegation's body is one `Try`.
  Probes: target in `try` → 1; wrapper in `try` → 0; delegation in `try` → 0.

## Tests

- `middle_man_exempts_the_encapsulation_shapes` — the four exempt fixtures
  (each fires against the pre-change code, verified with a binary built from
  HEAD).
- `middle_man_keeps_the_delegation_this_codebase_can_answer` — the real
  delegation still fires once, and an interface name collision whose arity
  does not match is NOT conformance.
- `middle_man_delegation_detected` — kept; its first fixture now carries a
  resolvable target (`self.inner = Inner()`), since an attribute with no type
  evidence is exactly the case that no longer fires.
- `middle_man_resolves_a_target_in_another_file` — the cross-file corpus: a
  wrapper whose `Inner` lives in another file still fires (the per-file cut
  was silent here), and a library target stays silent.
- `middle_man_single_buffer_is_conservative` — `scan_source_impl(.., false)`
  (the LSP typing path) stays silent alone and fires with the target in the
  same buffer.

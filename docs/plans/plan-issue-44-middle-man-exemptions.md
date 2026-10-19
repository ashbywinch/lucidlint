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

`module_middle_man_classes` reads the file's classes once: whether each is an
interface (`ABC`/`ABCMeta`/`Protocol` base or an `@abstractmethod` member),
its methods with their parameter shape (positional-required count, names with
defaults, every non-receiver name), its bases, and which module class each
`self.<attr>` is built from (an annotated field, `self.x: T`, or
`self.x = T(...)`).

A single-statement `return self.<chain>(...)` then fires only when ALL hold:

1. the chain resolves to a method the file's own classes define (walking the
   receiver class and its module-defined bases) — otherwise the target is
   outside the scanned file and the wrapper is the adaptation point;
2. no interface the file declares has a member of that name whose parameters
   the wrapper accepts (a conforming implementation, not a name collision);
3. the call does not fix policy — no keyword the target defaults, no
   positional beyond the target's required ones (`**mapping` is forwarding,
   not policy).

`self_call_chain` replaced the old `is_self_call`: it returns the attribute
chain, so the receiver class is resolvable rather than assumed.

## Boundary (recorded, not a defect)

Resolution is per FILE, not repo-wide: a target defined in another module of
the same repo is treated like a library target. Both houses removals that
this affects are exempt for other reasons anyway (rule 2/3 would clear them
too), so the two-repo outcome is identical to a repo-wide index — at a
fraction of the machinery. The interface check is likewise file-scoped
(interfaces are declared beside their implementations in these repos).

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

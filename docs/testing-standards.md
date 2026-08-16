# Testing Standards

Standards for testing: what to test, how to write tests, and how the test
suite is organised.

## Type checking is a first-class gate

The language's type checker is configured (strict where the toolchain allows)
and runs in the test gate — errors fail the build, never suppressed.
A `# type: ignore` requires a comment explaining why, and a suppression is
itself a finding.

## Write the test before the code

Follow test-driven development: a failing test first, then the code that
makes it pass (`rule://test-first`). For LLM behaviour, write evals instead
(real AND fictional fixtures, running the production path exactly once).

## Test properties

- Tests mirror module paths.
- Deterministic: no wall-clock, network, or order dependence; seeded randoms.
- Assert behaviour, not implementation.
- Sort file listings before processing — `glob` order is filesystem-dependent.
- A test that cannot fail on a plausible bug is not a test (no tautological
  fixtures — validate real artifacts).

## Never skip tests for a missing environment

A test that needs an external dependency — a store, a service, an API key,
a database — never skips when the dependency is absent. Fake it: a fixture
builds a minimal stand-in (a tmp store with synthetic data, a stub server,
recorded responses) so the test runs identically everywhere, CI included.
`@pytest.mark.skipif` on environment presence is forbidden: a skipped test
rots silently and the coverage gate lies about what is exercised.

Only the E2E suite may skip — a real external API is the one dependency
that cannot be faked faithfully (`@pytest.mark.e2e`, skipped by default;
see Organisation). Everything else fakes, with the fake injected per the
Mocking section (parameter injection first — never `monkeypatch` a module
constant to point at a tmp dir; make the dependency a parameter).

## Organisation

- **Unit tests:** one function/module in isolation, no API calls.
- **Integration tests:** full pipeline with fakes.
- **E2E tests:** real external APIs, one consolidated suite per API, skipped
  by default (`@pytest.mark.e2e`).
- Shared test infrastructure (fixtures, fakes) is extracted once, not
  copy-pasted per file.
- **Never let a derived artifact drift from a fresh run.** When something
  is both committed and derived (a projection, an index, a store a producer
  fills), its shape rule is enforced per [A described contract is not an
  enforced contract](coding-standards.md#a-described-contract-is-not-an-enforced-contract)
  — verified here against regeneration. Three tests:
  - the committed version equals a fresh run, compared on a defined stable
    form implemented as a normalizing function declared alongside the
    producer (canonical ordering, no timestamps, paths relative to the
    repo root, locale-independent) — never raw output, so a
    non-deterministic producer cannot make the test flaky; pin the
    normalizer itself with a unit test for determinism and environment
    independence;
  - re-running the producer changes nothing (idempotent), compared on the
    same defined stable form — incidental raw-output nondeterminism
    (ordering, timestamps) is not drift;
  - the invariant is asserted on the producer's declared inputs and
    outputs (a unit-level check, no live state required) and separately
    on the fresh-run result using the same defined stable form; both
    assertions must pass.
  Hand-patching the committed output hides a producer bug that resurfaces
  on every fresh build. Regenerability is a claim, verified not assumed.

## Precompute, materialise — unless the test is the round trip

Expensive derived data is precomputed once — and if it is stable, it is
materialised as a committed artifact so tests never recompute it at all.
The S1 finding is the pattern: the sampled phases were derived from the
live sessions once, committed as s1_dump.jsonl, and the tests read the
committed evidence.

- **Materialise when it saves time and the artifact is small enough for
  git** — a derived fixture (a dump, a snapshot, a generated corpus) is
  computed during development, committed, and read by the tests.
- **Do not materialise a gratuitously massive dataset** — a huge artifact
  bloats the repo and the clone; the tradeoff is a real cost, not a
  preference. When the dataset would be huge, rewrite the test to not
  need it: shrink the fixture to a representative subset, or restructure
  what is being tested.
- **Never materialise the output of a round trip the test exists to
  verify** — if the whole point of the test is that the current code can
  round-trip something (serialize → parse → compare, write → read →
  compare), the test runs the current code; a committed artifact would
  compare against a stale snapshot and the round trip would go
  unexercised. Materialise the input if helpful; never the round-trip
  output.

Within a test run the same principle applies at fixture scope: a
session-scoped fixture, a module constant, a parsed-once structure —
never recompute the same derived data per test. The suite's runtime is a
property, and a test that rebuilds what a sibling already built is wasted
work.

## Fake the filesystem before touching the real one

File I/O in tests uses pyfakefs wherever possible (the gate enforces it;
  add `pyfakefs` to dev dependencies).

- Write to fixed fake paths (`/store`, `/labels.jsonl`) — not pytest's
  `tmp_path`, which breaks under the fs patch (`Path.relative_to` fails);
  fake paths need no cleanup anyway.
- Committed artifacts stay readable by mounting their real directories
  (`fs.add_real_paths([...])`), never by copying or by falling back to
  the real FS.
- Reach a real `tmp_path` only when the code under test needs real
  filesystem semantics (subprocess interop, symlinks, OS-specific
  behaviour) — and comment why.

## Mocking and dependency injection

Inject the dependency (service, output path, fake) — never
`monkeypatch`/`patch` global state. If something isn't reachable through DI,
refactor the code to accept a dependency. Fakes subclass the real protocol so
the type checker still holds at edit time.

Injection patterns, in order:
- **Parameter injection** for leaf-level dependencies (underscore-prefixed
  optional param falling back to the real implementation).
- **Services container** for deep call chains.
- **ContextVars** for request-scoped singletons.

Forbidden: `monkeypatch`/`unittest.mock.patch`, module-level mutable state,
lazy imports, and abstract base classes with a single concrete
implementation.

Pure functions need no mocking. Function-param injection is next. Containers
after that. Global patching is a last resort and a smell.

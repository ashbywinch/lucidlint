# Global Coding Standards

Language-agnostic by design; per-language toolchain conventions (ruff, eslint, formatters, type checkers, semantic-type libraries) live in the scaffold skill's language layers, not here.

## Design principles

### Structure & Organisation

- **Separation of concerns.** One reason to change per module/class/function.
  HTTP vs business vs persistence live in different modules with one-way
  dependency chains; importing a sibling layer directly is a finding.
  **Never mix I/O with computation** — a function that fetches, reads, or
  parses and then decides or computes in the same body is a finding; the
  split is I/O in, decision out. **God functions are a finding** — one body
  that reads, transforms, persists, and formats does four jobs and changes
  for four reasons. A PR that touches a module for two unrelated reasons is
  the module having two reasons to change.
- **Cohesive modules and classes.** Data + behaviour together. A module/class
  owns its invariants; external code never reaches into structures to compute
  derived values. Public-field classes that others manipulate are
  poorly-organised dicts; accessor chains feeding procedural code are missed
  abstractions. **Function strewing is a missed class** — when several
  functions share the same data (the same record, the same fields), they are methods on that class. A module that is a pile
  of functions over one structure is a finding, not a style preference.
  **Name the class for the domain concept, and model the single record.**
  When a family of functions over a record becomes a class, the class is
  named for what the domain calls the collection (`Labels`, `Findings`),
  never a pattern noun (`LabelStore`, `LabelsRepository`, `LabelsManager`),
  and the single record is a first-class type alongside it (`Label`) — the
  class owns the collection's invariants, the record is the thing itself.
  **No module-level mutable state and no `global`** — put state in a class
  named for the domain concept it represents (`_APIState`,
  `_GeocodeRateLimit`), never the pattern name (`_MutableState`).
- **Groups that travel together are a type — never repeated parallel
  parameters.** The same set of fields passed to several functions is one
  object: `assess(people, places, themes, items)` repeated across callers is
  the anti-pattern; a `Knowledge`-style object with named members is the fix.
  Data clumps in signatures become domain classes (`GeoPoint`, not
  `LatLngPair`), and the functions that operate on the clump become its
  methods.
- **Each class in its own module.** Named after the class; exception for a
  module grouping closely related handful-of-fields models that share one
  reason to change. Extract once a class grows non-trivial behaviour.
- **No over-abstraction.** No abstract base classes with a single concrete
  implementation, no plugin systems, pipeline frameworks, or service buses —
  write straightforward functions that call each other. Protocols or
  `Callable` aliases are fine for defining a seam; a hierarchy with one leaf
  is ceremony.
  **Imports.** Inline imports hide dependencies from static analysis (the gate
The gate flags them). Private (underscore) symbols from another module are never
  imported — make the logic public or extract it to a shared module.
  Circular imports are fixed by restructuring modules, never bodged with lazy
  imports.
  body (inline imports hide dependencies from static analysis). Never import
  private (underscore) symbols from another module — make the logic public
  and documented, or extract it to a shared module. Circular imports are fixed
  by restructuring modules, never bodged with lazy imports.

### Code and structured data live in files

- **A string literal never carries code or a structured format as text.**
  Code that is meant to be executed, parsed, or maintained as code — a
  snippet, a template, a generated program — is a real file. So is a
  string whose content IS the data format: a JSON fragment, a CSV row, a
  YAML blob, hand-written in a literal, is invisible to the format's own
  validation (a parser, a schema), drifts from the real toolchain, and
  cannot be linted or run. Building the text with the format's library
  from program structures is not a violation — `json.dumps({...})`,
  `csv.writer(...)` — the text is produced, never hand-written. In tests,
  a fixture that is code, or structured data the test needs as text, is a
  real file under `tests/fixtures/`.

### Naming & Types

- **Names communicate intent.** Domain names, not shapes: `monthlyPayment`
  not `calculateValue3`. A name needing a comment is a failed name. Classes
  are **domain nouns** — a concept from the domain the app is built for, the
  world the users/customers live in, that a domain expert would recognize —
  never a noun from the technology under the hood. Turning a verb into a
  noun (`GraphBuilder`, `FileReader`, `DataFetcher`) is cheating: the real
  class is the domain concept the verb operates on (`RelationshipGraph`,
  `Letter`, `Feed`) — "geocoder" is a smell, the geocoded *place* is the
  noun. Functions are verbs; variables hold what they name (`price`, not
  `x`); booleans read in `if` (`hasSchool`, not `schoolFlag`); an id is never a label
  **The docstring test:** a docstring that merely rephrases the name
  (`TransitOrchestrator` → "orchestrates transit") means the name or the
  concept boundary is wrong — fix the name or split the concept. **The
  stress test:** would someone who knows the domain but not the code
  understand this from the name alone? **The object model is the naming
  authority:** where a TECHSPEC documents one, a class or module whose name
  is not an object-model noun is a finding — either the model is missing an
  object or the name is wrong; fix one, never let them drift. Docs may carry
  process vocabulary ("the classify step of the pipeline"); code names may
  not.
- **Semantic types over primitives.** A point in time is a date type with
  awareness, not a bare string; structured data is an object with named
  fields, not a bare dict; enumerated values are enums; units are part of the
  value; money is a **Money type carrying its currency** — never a bare float
  and never a bare `Decimal`. Before reaching for a primitive: "is there a
  type that makes this impossible to misuse?" **Coerce untyped data** (API
  responses, parsed files) to the structured types at the boundary —
  immediately inside the layer that receives it, never deep in business
  logic. **Wire formats are the exception, explicitly:** serialized payloads
  and config files use unit-named bare values (`threshold_min`,
  `duration_min`) by design; the semantic types govern all in-memory
  computation, and conversion happens only at the (de)serialization boundary.
  **Never float-compare monetary values, even in tests** — wire strings parse
  via `Decimal` (exact: `35500.00 − 25500.00` is `10000.00`, not
  `9999.999…`) into the Money type, and tests compare Money, never floats.
- **Quantities carry their units.** A distance, duration, speed, or weight is
  a typed value with its unit attached — never a bare number. Before
  reaching for `float`/`int`, ask "what unit is this in?" — unit-named field
  names (`duration_min`) are a smell that the type is missing. The library
  choice (pint for quantities, a Money type for currency) is the toolchain's
  call; the principle is not.
- **Don't let output format drive the domain model.** Values that appear
  together in a report or API response are not necessarily the same domain
  concept. Think carefully before bundling them.
- **Never hand-roll parsing where a library or a structured assertion
  exists.** A bespoke regex date scraper when the answer is "the model
  asserts what a value is, the library parses it" is the anti-pattern.
  Dates, numbers, formats: let a validated library parse; the code asserts,
  never invents.
### A described contract is not an enforced contract

A contract that lives only in a sentence is missed exactly where it
matters: models pass happy-path tests while the stated contract is unmet
(ContractEval, ACL 2026 Findings,
https://aclanthology.org/2026.findings-acl.2112/). A shape rule is
enforced by a *type, a validator, or an eval* decided where the data
enters, with a test pinning it in the same change. A rule in prose
without a check is a wish:

- ✗ "The record's end date must be after its start date."
- ✓ `Record` is a type whose constructor rejects an end date before
  `startDate`, and a test pins that rejection.
- A validator that returns success when a required datum is missing is a
  finding: the fields the rule reads must be complete on the record, and a
  missing datum is a data gap that fails validation — never special-case
  the validator into guessing or skipping.
- **A rule that must hold across a collection names every surface it
  reads.** A fact can be expressed on more than one surface (a record's own
  text, a document that refers to it, a note); a cross-collection rule
  names every surface where the fact can be expressed, or it silently
  misses whole classes.

### Robustness

- **Anti-fragile: correct by construction.** Types make invalid states
  unrepresentable; pure functions preferred; error paths explicit — in the
  type system where possible (discriminated unions, not `None`); never cast
  suppress type-checker flags (`# type: ignore` must have a reason — the
  gate enforces it)  explaining why); happy paths read naturally. **Never mutate function
  arguments** — if a function must change a dict or list, create and return a
  new one; in-place mutation makes call-site behaviour unpredictable. Signs of
  coincidental correctness: works only in your test env, reordering "happens
  to work", unrelated breakage, unwritten rules ("always call X before Y").
- **Fail fast, never swallow errors.** Every `except`/`catch` must log,
  re-raise, or handle observably. No empty `except` blocks — catch specific
  exceptions and at minimum log them. **Silently logging is not an
  implementation of fail fast** — a failure must be user-visible: propagate
  it, surface it, or emit the two-tier messages. The only case where logging
  alone is acceptable is an explicitly safe-to-ignore error, and it logs at
  `DEBUG` with an explanation of why it is safe. Invisible failure is a bug:
  ```python
  # ✗ invisible
  try:
      do_something()
  except Exception:
      pass
  # ✗ logged but still invisible — the user sees nothing and the job "succeeds"
  try:
      do_something()
  except Exception as e:
      logger.debug("do_something failed (non-fatal): %s", e)
  # ✓ observable: re-raise, or surface a two-tier message to the user
  try:
      do_something()
  except Exception as e:
      logger.error("do_something failed: %s", e)
      raise
  ```
  **A warning that still proceeds is wasted code** — if a condition is bad
  enough to warn about, it is bad enough to fail. **Don't pre-validate before
  trying** — let code fail naturally (a missing key surfaces as the API's 403,
  not a bespoke pre-check). Exception: interactive/CLI setup flows whose
  natural failure is misleading may pre-check configuration, and must then
  emit the two-tier messages.
- **Two-tier failure messages.** A fail-fast path triggered by environment,
  configuration, or user error emits two messages: a plain-language user line
  (what happened + what to do next, no internal identifiers, no stack traces)
  and a dev log (root cause + exact resolution). One half alone is a bug.
- **No backward-compat shims.** Delete the old path; migrate callers in the
  same change. No aliases, no re-exports, no "will remove later". **Dead code is deleted, not kept** — the git log is the archive. If you would write
  a comment to explain a block, extract it into a named function — the name is
  the documentation.
- **One way through for invariants — never parallel paths.** When a store or
  module has rules (append-only, supersede-don't-delete, a validation gate),
  those rules live in exactly one owning module, and every path that touches
  the data goes through it. A second path that writes the store directly, or
  a new caller that re-implements the rule, is a finding — it creates a
  bypass where the invariant is not enforced. The owning module enforces the
  rule in code, and a test double that fails on a violation proves it:
  ✗ every caller writing the append-only store directly, hoping they
  remember not to overwrite. ✓ one `Archive.save_item` that supersedes, with
  a `MemoryStore` that raises on any write to an existing file.

### Data & State

- **Real content never lives in code.** Data a user provides or states, and
  configuration values, live in data/config files — not hardcoded in source.
  Code is the pipeline that reads and transforms them. A hardcoded real
  record (a person, a fact, a relationship) forks the data into the codebase,
  where it drifts, gets hand-edited, and can be lost. Secrets are the sharpest
  case: environment only — never read, log, print, echo, or store keys in
  context, files, code, output, cache keys, URLs, or request bodies (headers
  only), and redact keys from error messages before logging.
- **Derived data is regenerated, never hand-edited — and a test proves
  it.** Any file computed from other data (generated indexes, projections,
  summaries) has exactly one writer; hand-editing it is a bug, and a test
  pins the committed output to what the generator produces, so drift fails CI
  instead of silently diverging. A derived cache is never a migration source:
  bootstrap scripts read the source of truth, never a regenerable file a
  previous run may have polluted.
- **Dev is the environment, not the quality bar.** Code written during
  development is the code that ships — production standard from day one,
  including spikes and prototypes. There is no "throwaway" quality tier: a
  spike that survives is the production code, so it is written as such. The
  only thing dev changes is where data lives and which external services are
  real.
- **Partial output is a resume strategy, never an accident.** Broken partial
  output — a truncated file, a half-written store, a run that silently stops
  mid-way — is a bug. Intentional partial output, written as part of a
  resumable checkpointing design, is fine: **long operations are resumable if
  they crash** (idempotent, checkpointed, re-runnable from the last
  committed point) **and troubleshootable after a crash or a long run**
  (the state shows exactly where it got to; logs cover the failed stretch).
  Derived or canonical outputs are still written atomically (temp + rename)
  or gated behind success — a failed run never publishes a partial result
  *over a good one*. Never bulk-clear or regenerate a surface holding manual,
  user-entered, or curated data; destructive writes are scoped to exactly
  what is known better.
- **UTC datetimes: aware, explicit boundaries.** Store and process UTC —
  never `datetime.now()` without a timezone. Display local time only at the
  presentation boundary (template, API response). Document an external
  source's timezone and convert explicitly to UTC before storing. After
  parsing from a DB or file (`fromisoformat` may return naive), check
  `tzinfo is None` and fix it. Naive↔aware comparisons raise; arithmetic is
  wrong across DST.
- **Cache key hygiene.** Never include API keys or tokens in cache key
  parameters (rotation must not invalidate the cache). Never cache non-OK API
  responses — a transient failure (rate limit, bad key) must not poison the
  cache.

### External Interactions

- **Batch external API calls wherever possible.** A loop that calls an
  external API once per element is the anti-pattern — batch where the API
  allows it (bound the batches, map responses back in order), and where it
  does not, cache and reuse within the run instead of repeating the same
  call.
- **Follow established practice for well-trodden surfaces.** Chat,
  autocomplete, forms, REST APIs — solved problems with documented
  conventions (REST: nouns for resources, plural collections, query params
  for filtering, non-CRUD actions as sub-resources or PATCH state changes).
  When a surface has been done before, research the convention first and
  follow it; record the research and any deviation with a named reason and
  date. ✗ inventing a novel layout for a standard widget.

### Process & Tooling

- **Every `python -m` entry point forwards its argv.** A module whose
  `main(out=None)` defaults to a real path but whose `__main__` ignores
  `sys.argv` writes to the default silently — a "dry run" into a temp dir
  actually touched real output. ✓ `main(Path(sys.argv[1]) if len(sys.argv) > 1 else None)`.
- **Deterministic checks beat model judgment; AI output enters as
  proposed.** A rule that can be checked in code is enforced in code, in the
  same loop — the LLM review pass is only for what code cannot check. Machine
  output (extractions, generated text, classifications) is never silently
  treated as fact: it enters as `proposed` and a human confirms it in the
  same flow's review. Never present machine output as a person's own words.
- **Boring over clever.** Prefer the obvious implementation a future reader
  can understand in any editor. Correctness first, then the next maintainer.
  Prefer libraries over reinvention — a library call that replaces 30 lines
  of well-known computation is worth it; one that adds more complexity than
  the code it replaces is not.

# lucidlint language conventions

## Python conventions

- Web servers default to FastAPI + uvicorn when an HTTP surface exists; the
  framework owns request parsing (houses rule).
- uv + `.python-version` pin; ruff `select E,F,I,UP,B,SIM,N`, `line-length
  120`, double quotes, no ignore list.
- pytest with `testpaths = ["tests"]`; dev deps in `[dependency-groups] dev`.
- pyrefly with the both-direction baseline lock
  (`scripts/pyrefly-lock.py` — new errors AND stale entries fail); the
  `[errors]` table enables `missing-override-decorator` + `missing-super-call`.
- Semantic types: pint for quantities, a Money type for currency (never
  bare float/Decimal in signatures).
- Raw git hooks delegate to make (lint + typecheck + gitleaks); pre-push
  runs `make check` — hook and CI can never drift.
- The gate (`lucidlint.py`) is the house finding engine for Python code:
  every finding family computes in the Rust scan core; the orchestrator
  converts the versioned contract and renders.

## Rust conventions (scanner/)

- Toolchain pin: `scanner/rust-toolchain.toml` — the exact stable channel;
  CI and local rustup read the same file. A bump is deliberate and re-runs
  `make lint` (clippy's diagnostic set can change).
- Lint gate: `cargo fmt --check` + `cargo clippy --all-targets -- -D
  warnings` inside `make lint-check` — deterministic under the pin, so no
  baseline. An `#[allow(...)]` carries a reason comment; no blanket
  `#![allow]`.
- Type gate: `cargo check --all-targets` inside `make typecheck` — the
  borrow checker is the type checker; errors are fixed, never suppressed.
- Format: rustfmt, `max_width = 120` (matching `.editorconfig`); one
  formatter per artifact — generated code is owned by its generator, never
  re-formatted.
- Coverage: `cargo llvm-cov --lcov` → `lcov.info` (gated in CI alongside
  the Python coverage).
- Semantic types: newtype wrappers over primitives (units in the type
  name — `Meters`, never bare `f64`); conversions at the boundary.
- Modules: the scan-core modules (`checks`, `graph_families`, `docs`,
  `lsp`) are standalone — only `main` composes them. Enforced by
  `scanner/tests/layers.rs` (exact-path assertions).
- The graph contract (`lucidlint_graph_export.py`) is the code-review-graph
  interface: generated through the tool's own public API, versioned, never
  the SQLite schema or location.

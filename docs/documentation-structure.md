# Documentation Folder Structure — the required doc set

Standards for what every project's `docs/` folder must contain and how it is
organised. Separate from `docs/writing-documentation.md` (what good
documentation is) — this doc is the structure: which docs exist, where, and
what each must contain. The `skill://write-documentation` skill is the
process that applies both.

## The required doc set

Every project's `docs/` folder carries three documents, and the review bot
checks PRs against them. Each is **itself held to the documentation-quality
checklist** (`docs/writing-documentation.md`) — a PRD, TECHSPEC, or PLAN
that fails context efficiency is a finding, not a formatting preference.
Each may split into a subfolder (`docs/PRD/`, `docs/TECHSPEC/`,
`docs/PLAN/`) with subsections when big; the index doc keeps the canonical
path.

### Requirements — `docs/PRD.md` (or `docs/PRD/` when big)

The product requirements document. A PRD that needs splitting into
subsections should have one main PRD for the whole product and sub-PRDs for
phases, features, or user stories — slices of the product that will be
delivered together. Sections, in order:

- **JTBD first** — a comprehensive set of very high-level Jobs To Be Done,
  the opening section. May include emotional JTBD ("feel the family is
  remembered"), not only functional ones.
- **Personas** — a small group, specific enough to drive decisions. A
  persona is decision-useful when two plausible product choices differ
  under it; a persona that cannot change a decision is filler.
- **Constraints that shape everything**: cost to run, longevity of the
  system, backups, monitoring and tracing once in production, access and
  auth, likely scale at different phases (data size, number of users, …),
  and hosting limitations — internal or external — stated explicitly when
  either is off the table.
- Big PRDs split into subsections (`docs/PRD/`) instead of one wall; each
  subsection keeps the density rules.

### Technical spec — `docs/TECHSPEC.md` (or `docs/TECHSPEC/` when big)

How it will all be done. Sections, in order:

- **Component breakdown** — every component/module with its responsibility,
  its interfaces (what it provides and consumes), and the data it owns.
  Components are the code's boundaries before the code exists.
- **Object model** — the domain nouns (and analytical objects: findings,
  metrics, runs) with purpose, key fields, and relationships. It is the
  naming authority for the implementation: a class or module whose name is
  not an object-model noun is a finding. See `skill://tech-spec-writing`
  for the process.
- **Architecture first** — the architecture layers as a mermaid diagram, plus
  data flows and physical architecture. Reference the non-functional
  requirements from the PRD (cost to run, longevity, backups, monitoring,
  auth, scale, hosting) to explain why the architecture is shaped the way
  it is. Multiple diagrams are fine, one per concern.
- **Technology choices** next, with the alternatives considered and why they
  lost. The architecture determines what choices make sense, not the other
  way around.
- **Spikes required to confirm** — named, with what each must prove before
  the choice is locked.
- **Strategic technical decisions** that follow from requirements —
  reference the requirement by name and content, not only by number
  (numbers change).

### Plan — `docs/PLAN.md` (or `docs/PLAN/` when big)

The intended phases:

- Each phase states its **inputs, outputs, operations, and the quality
  gates of the app after that phase**. The inputs and outputs are the
  **software's** at that phase, not the project phase's: inputs are what
  the app consumes (data, users, artifacts it reads), outputs are what the
  app produces (capabilities, results, artifacts it writes) — never the
  phase's project-management artifacts ("docs written", "code landed").
  The quality gate is the app behaving correctly at that point, not a
  project deliverable being "done". A phase is complete when the app
  passes its gates.
- **Phases never depend on outputs of later phases** — every phase ships
  in order.
- **Phases are ordered for earliest user value** — each phase is the
  minimum possible addition that drives real value.

## Discoverability

**Every doc must be discoverable from AGENTS.md** — directly or by
following links (one level deep is the norm). A doc that AGENTS.md cannot
lead to is undiscoverable, and undiscoverable documentation is a finding:
it does not exist for the reader who starts where all readers start.

**Skills are exempt from the AGENTS.md reachability rule.** A harness
loads skills and tools through its own built-in discovery mechanism — a
skills tool, a registry, an index — and that mechanism varies from
harness to harness. Findability is the harness's job, not the doc
tree's: the reachability rule above covers the documentation set
(`docs/`, `standards/`, `rules/`), and a skill the harness's own
mechanism can find needs no link from AGENTS.md.

## The standards the folder carries

A project repo's `docs/` also carries the standards the review bot enforces —
`coding-standards.md`, `testing-standards.md`, `writing-documentation.md`,
`ux-standards.md` (and this file) — copied from omp-config's `standards/`
(coding, testing, UX) and `docs/` (writing-documentation,
documentation-structure) per the repo-scaffold skill. In omp-config itself,
`docs/` carries only what governs it — this file, `writing-documentation.md`,
`PRD.md`, `standards-deployment.md`. This file is about the project docs they
mandate.

# UX Standards

Standards for the user experience of an app. Principles are stated as user
outcomes and behaviours — deliberately NOT as UI specifics (labels, buttons,
placements). Implementation detail and the record of UX decisions live in the
repo's own UX documents. Surface-specific standards (chat surfaces, capture
flows) live in the repo alongside this standard. The skills that run UX work
(`skill://user-interviews`, `skill://ux-process`) reference this document;
they do not restate it.

Principles are stated as **user outcomes and behaviours — deliberately NOT
as UI specifics** (labels, buttons, placements). Implementation detail and
the record of UX decisions live in the repo's own UX documents (a
usability-requirements baseline and a UX-fixes/decisions plan — see
`skill://ux-process`). When the UI changes, test against the principles
first: a requirement may be satisfied differently, but a violated principle
is always a regression.

## Principles

**P1 — Facts in, consequences out.** The user enters only their own facts.
Everything derived (calculations, summaries, predictions) is computed from
those facts once, consistently everywhere, and updates automatically when a
fact changes. Anti-patterns: recalculate actions, per-item re-entry, stale
numbers, client-side re-derivation.

**P2 — Every claim is true at face value, explainable one step away.** A
displayed figure or label asserts something about the world. At a glance it
must not mislead (what it is, whose it is, and that a failed/unknown value
is never presented as real); on demand, the rest of the claim — what it
depends on, how it was derived, why it's missing — is recoverable within one
step, never buried a second click away.

**P3 — The system surfaces failure; the user fixes facts, not symptoms.**
Missing or wrong data is the normal state. The app names what's missing in
plain words and the fix operates on the user's own facts, never a workaround
or a dead end.

**P4 — Organize by the user's world, not the system's.** The UI's structure
mirrors how the user thinks about their life, not the internal model
(records, nodes, modules). Internal words stay internal.

**P5 — Change is a core journey, not an admin task.** The app's value is
that it stays right as life changes. Editing is discoverable from where its
consequences appear, and every edit has a visible, immediate consequence (a
closed feedback loop).

**P6 — The baseline is behaviour, enforced by machines where possible.**
Requirements are outcomes, not pixels. Where a behaviour is checkable it
gets an automated test; the walkthrough proves the experience (P13).

**P7 — Real states are explicit, never approximated.** Every real-world
state a user can be in is a first-class, explicit control or field — never
encoded as a combination of other inputs, never silently inferred. Inference
is allowed only as a one-time migration to an explicit value. Anti-patterns:
zero-values-as-meaning, forcing the user to reverse-engineer a state out of
unrelated inputs.

**P8 — One standard affordance per pattern.** Every recurring interaction is
served by exactly one reused, standardised component across the app.
Consistent affordances are learnable; per-screen variants are invisible
until discovered.

**P9 — Use standard UX patterns.** Authentication, navigation, chat, and
other well-trodden surfaces use standard conventions (header menus,
drop-downs, established chat patterns) that a first-time user already
understands from the web. Research the convention carefully and document it;
deviate only when there is a concrete reason. A bespoke navigation scheme
or an original authentication flow is a finding unless the standard pattern
cannot solve the problem.

**P10 — Model only distinctions that change behaviour or display.** Every
user-facing category, toggle, or field must change what the app does or
shows. Taxonomy that exists only to be edited is cost.

**P11 — No bootstrap deadlocks.** A first user must be able to make the app
useful without a pre-configured admin or identity linkage — a self-service
identity-claiming path or a guaranteed first-run flow.

**P12 — User-entered data is never silently lost.** Updates merge into
existing records; a partial edit never resets unrelated fields; writes from
outside the app are guarded and explicit. Prefer append-only patterns (a new
record supersedes, never overwrites) and always use soft deletes (a deleted
record is hidden, not removed) — the same principle as the append-only store
rule in the coding standard.

**P13 — UX work is accepted by re-walking the scenarios.** A repeatable
walkthrough instrument (`skill://ux-process`) is a living test: usability
changes land only when the scenario walkthrough no longer produces the
confusions they targeted. Unit tests prove mechanics; the walkthrough proves
the experience.

## Language

- **Outcomes language, never task language.** Frame the product in discovery
  outcomes ("find a relative", "save a story") — the word "task" reads as
  chore. Instruction bars that say "complete metadata" are wrong twice.
- **Two experiences, one app.** Separate novice and deep doors structurally:
  the novice path front-doors, the deep path (search, dense tools) is
  available but not the landing. One experience cannot serve both expert and
  novice.
- **Explorable, not editorial.** The app arranges the user's material and
  never interprets it. Presentation can be warm and serendipitous (Google
  Photos Memories is the reference) — emotional presentation is not
  editorializing.
- **Controls are the trust layer.** Anything emotionally loaded (memories,
  sensitive content) is safe only because the user controls it — hide,
  filter, exclude. The controls are visible and first-class.

## UI conventions

- **One style path.** New UI goes through the repo's established components
  and semantic tokens; a second, parallel style is a finding (the
  design-system counterpart of "one way through for invariants" in the
  coding standard).
- **Touch targets ≥ 44px** on the primary device; colour contrast on the
  app's palette; respect `prefers-reduced-motion`.
- **The primary action stands out.** The one action that ends/advances a
  flow is visually primary; supporting actions are quiet; a destructive
  action is explicit and confirmed.
- **Surface-specific standards live in the repo.** Chat surfaces, capture
  flows, and other recurring patterns get their own documented standard
  (e.g. a CHAT-UX standard), which the components layer beneath it — same
  rule as the code modules.

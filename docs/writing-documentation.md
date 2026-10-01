# Documentation Standard — what good documentation is

Standards for documentation quality — what good documentation looks like.
Separate from `docs/documentation-structure.md` (which mandates what docs
must exist and their folder structure). The `skill://write-documentation`
skill is the process that applies both.

## Skills are documentation

A skill (`skills/<name>/SKILL.md`) is documentation: it is a doc written
for the agent that loads it, and it is subject to every rule in this
standard — context efficiency, density, no-duplication, one topic per file
(a skill covers one task or procedure; a second task means a second skill),
the quality checklist, and the always-loaded size ceilings (a skill body is
loaded in full when used, so the 150–200 line / 32 KiB targets apply to it
directly). The review bot checks skill changes against this standard like
any other doc.

## Context efficiency

Every documentation file contains only information relevant to its topic
and audience. Each doc has a single topic and a single audience. Content
that belongs to a different audience or topic goes in that doc, linked —
never copied in.

All documentation must be usable by humans or by AI agents; both navigate
from the entry point of AGENTS.md. Skills are exempt from the reachability
rule — a harness discovers them through its own mechanism (rationale and
scope in docs/documentation-structure.md).

## AGENTS.md is a bootloader, not an operating system

AGENTS.md loads everything else and gets out of the way. It holds the quick
start, the decision tree that routes every task to the right doc, and the
rules that apply to **every** agent in the repo — nothing more. Content that
only some agents need belongs in the doc the tree links to, never in
AGENTS.md itself. Test: is this genuinely relevant to 100% of agents
working in this repo? If not, it does not belong in the bootloader.

## Single source of truth — never duplicate

Each piece of information — whether documentation or code — lives in exactly
one place. Other docs link to it; they do not repeat it. A duplicated fact is
a finding: the copies drift, and the reader cannot tell which is current.

**Do not duplicate the code's job.** A doc that restates a function signature,
a default value, or a class structure is waste — the code is the source of
truth for those. Reference the code instead ("call `compute()`; see the
function docstring for the `Attempt` contract").

When tempted to copy content into a second doc, link instead — the link is
the duplication-free way to reuse.

**Good:** the development guide says "See the column reference for
details" and links to column-reference.md.

**Bad:** the development guide repeats the column layout inline — two
copies, one of which will go stale.

## One topic per file — the docs' separation of concerns

One reason to change per doc file, exactly as one reason to change per
module (docs/coding-standards.md 'Separation of concerns'): each doc
covers one topic for one audience. A doc with two audiences or two
unrelated topics is a finding — split it and link. A subtopic for a
different audience is a separate file that links back. The question
before writing or editing: "what single question does this doc answer?" —
a second question means a second file.

## Avoid redundancy

Before adding content to a doc, check whether it already exists elsewhere.
If it does, link to it. If it does not, put it in the most logical place
and link from the other docs.

## Delete, don't archive

Obsolete content is a liability. When something is no longer accurate,
delete it. Don't rename it "legacy", don't add a deprecation notice. If
it's wrong, remove it.

## Docs must match the code

When you rename a function, module, or tab, update the docs in the same
commit. Outdated docs are noise. Not every feature needs its own doc — a
well-named function with a clear signature and a readable implementation
is often its own best documentation. If the code is easy to read and the
interface helps users get it right, a separate doc may be redundant.

## Prefer automated tests and readable code over documentation

Where possible, write an automated test that proves the behaviour instead
of a doc that describes it. Prefer interfaces that make it easy to get right
(intuitive names, strict types, sensible defaults) over documenting how to
use them. The test is a living doc that fails when it lies; a written doc
stays green when it goes stale.

## No lectures on history

Never explain why a decision was made by describing the alternative that
failed and the person who made it. "It's a constraint inherited from X's
code" is a lecture. The reason is what matters; the history is noise. If
a reader needs to know why, state the constraint: "this message format is
required by the upstream API" — not "Bob chose this format in 2022 because
the old parser couldn't handle".

## API keys never go in docs

API keys, passwords, and secrets never appear in documentation or `.env`
files. They live in the shell environment only.

## Density & concision

Docs are read inside a limited AI context window; every sentence costs
context. Write for density: the smallest set of words that preserves every
fact and decision.

- **Rules as explicit negatives.** "Never X" reads faster and is followed
  more reliably than "be careful about X". (For instructions an agent must
  *act on* — a skill, a rule, a task prompt — see `skill://prompt-craft`.)
- **Commands over prose.** `make test` beats "run the test suite to verify
  everything works".
- **Tables over prose.** A rule per row beats a paragraph per rule, when a
  fact has consistent fields.
- **Canonical ✗/✓ examples over exhaustive enumeration.** One right/wrong
  pair teaches more than a list of edge cases; never enumerate every
  failure mode, show the pattern.
- **One-line contracts.** `compute()` MUST return an `Attempt` — prefer one
  line over three sentences.
- **Decision-relevant context only.** Keep only background that changes a
  decision; cut filler, restated motivation, and restated rules.
- **Size ceilings.** Always-loaded files (AGENTS.md, CLAUDE.md, skill
  bodies) target ~150–200 lines / <32 KiB — loaded in full every session,
  so bloat is paid every session. Referenced docs can be longer, but
  densify prose first.
- **Link, don't paste.** A fact lives in exactly one place; other docs link
  to it. References one level deep — a doc points to another doc, not
  through a chain.
- **Task-shaped sections.** When a doc describes how to do something, use
  the task-card shape: goal (one verb), scope (exact paths), constraints
  (must / never), acceptance (verifiable command).
## Clarity — the reader acts on the first pass

A doc that cannot be acted on after one pass has failed; a claim the reader
cannot check has no force. Every sentence passes two tests: does the reader
know what to DO, and could a reader who disagrees verify the claim against
the code?

- **Define every term at first use, or don't use it.** "The erasure is the
  finding" teaches nothing: neither word has a plain meaning in its sentence
  yet. If a concept matters, say it in one plain clause once — "the dict
  hides the value's real type" — or cut the term. Never borrow a term from
  the tool's internals, another doc, or a prior message without glossing it
  here.
- **One idea per sentence.** One subject, one verb, one instruction. An
  "and"/"but" chain or an em-dash clause is two sentences; split it. A
  sentence the reader must re-read is a defect.
- **Mechanism first, action last.** Name what actually breaks or drifts (the
  mechanism), then end with the single imperative. A verdict without a
  mechanism ("serialisation is no excuse") invites the reader to defer or
  dismiss; a mechanism without an action leaves them guessing. This is the
  tool's R31 and the plan rule "judgement passages state the operative
  mechanism and the reason" applied to every doc and message.
- **No excuses the rule doesn't own.** A categorical rule has no "unless
  it's genuinely X" tail. If a case falls outside the rule, change the rule;
  never offer a courtesy out in the message. (There is no "genuine
  configuration" exemption for module-level state: configuration is state
  and has an owning class.)
- **No deference to prior verdicts.** "Agent X called this nonsense" is not
  analysis. Restate the claim, check it against the code, take a position.
- **Verbs over nouns; active voice.** "Type the parameter with that class"
  beats "the parameter is typed as a record erasing the class". Eliminate
  "there is/are" openers. (Principles summarised from Google's Technical
  Writing One: specific verbs, one idea per sentence, terms defined at first
  use, consistent terminology.)
- **Short by saying less, not by compressing harder.** A plain two-line
  sentence beats a jargon one-liner; then shorten the plain one. Messages
  that must be concise earn that by dropping non-operative clauses, not by
  packing three ideas into one sentence.

Canonical pair:

✗ `data` is typed as dict but it is the serialised form of a record — and
the erasure is the finding: every call site can build the shape ad hoc, and
every field change happens unchecked, so the shape drifts away from the
class.

✓ `data` is a dict that holds the fields of an existing class (the wire form
its `from_dict` reads). Left as dict, call sites build the shape ad hoc and
field changes go unchecked. Type the parameter with the class.

## The documentation-quality checklist

- [ ] Single, clearly stated audience
- [ ] Single, clearly stated topic
- [ ] No content that belongs to a different doc
- [ ] No duplicated content from other docs (link instead)
- [ ] Every section is relevant to the stated audience
- [ ] Title and first paragraph make the purpose clear
- [ ] Links to related docs where readers might need them
- [ ] Every sentence carries a fact, a decision, or a constraint
- [ ] Rules are explicit negatives ("Never X"), not vague preferences
- [ ] Commands replace descriptions where executable
- [ ] Tables replace paragraphs where fields are consistent
- [ ] Code shows a canonical ✗/✓ pair, not exhaustive cases
- [ ] Every term is defined at first use (or not used)
- [ ] Sentences carry one idea: mechanism first, then the one imperative
- [ ] No escape hatch the rule itself does not state
- [ ] Always-loaded files within the ~150–200 line ceiling

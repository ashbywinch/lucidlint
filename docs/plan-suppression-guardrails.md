# PLAN — suppression guardrails: message wording, bulk-suppression visibility, guidance config, header

Workstream on the `review-log-improvements` branch (PR #5). Goal: steer
agents toward real fixes instead of non-fixes (value-spelled names,
mass per-site suppression) and make the repo's suppression policy
reviewable. Decision log: user approved A (messages), B (per-repo
guidance), C (census + bulk warning), and the report header; **D
(fixer-side refusal of value-spelled names) explicitly declined** —
naming stays a judgment call the tool must not second-guess.

## What is already done (committed or staged on the branch)

- **Message wording (A)** — the four abuse-prone families state the real
  fix AND the only legitimate suppression:
  - `magic-number` → "name it with a domain noun (what it means here),
    never its value spelled out" (both `main.rs` Python and `rustscan.rs`
    Rust emitters).
  - `record-shape` → "convert it to a class with named fields (wire
    formats at serialization boundaries are exempt)" (all 3 sites).
  - `broad-except` → "catch what you actually handle; a true boundary
    catch states its blast radius in the why".
  - `fakefs` → "...or mark `ignore-file fakefs <why>`, citing the
    standard that permits real FS here".
- **Report header** — `common::REPORT_HEADER` const + test: "the aim is
  code that is readable, maintainable, and obviously correct: fix
  findings instead of suppressing them, and give every suppression a why
  a reviewer can check". Present in the Rust JSON (`"header"` field);
  **must NOT appear under the LSP** (per-buffer scans, no report).
- **Bulk suppression (C, Rust side)** — `suppression_counts` +
  `bulk_suppression_findings` (warn, threshold 10, attached to the file
  with the most sites, message: "repeated identical whys are POLICY, not
  per-site judgment: move the rule into config guidance or a documented
  config ignore, or fix the recurring cause"). Wired into `main()` and
  the `scan_corpus` test helper. Catalog row `bulk-suppression` added;
  `make rules` regenerates rules_gen.rs + RULES.md (drift gate passed).

## Remaining work (in order)

### 1. Finish the Rust side — green suite
- [x] `bulk_suppression_warns_at_ten_or_more_sites` was panicking on a
      case-sensitive "policy" assertion against the message's emphasized
      "POLICY" — assertion lowercases before matching (test fixed, not the
      implementation).
- [x] Run the full `cargo test`; green (278 + layers).
- [x] `make rules` regen is idempotent with the tree + `make check` +
      `make self-check` green.

### 2. Header reaches the text output (orchestrator, lucidlint.py)
- [x] The Rust JSON carries `header` + `suppressions`;
      `RustFindings` now transports both (`lucidlint.py`), the TEXT report
      prints the header banner before the GATE line and the census in the
      footer (`suppressed: record-shape×408 fakefs×77 …`), and `--json`
      includes `header` + `suppressions`.
- [x] LSP path verified by live round-trip + `test_diagnostics_payload_
      never_carries_the_report_header`: no header in diagnostics.

### 3. Per-repo guidance config (B)
- [x] `[lucidlint.guidance]` loads from `.lucidlint.toml` and pyproject
      `[tool.lucidlint]`; `_LucidlintConfig.guidance` (default empty);
      apply_config appends `— house rule: <text>` to matching messages;
      unknown keys dropped against the catalog.
- [x] Tests: appended / absent / pyproject variant / unknown-kind-dropped.

### 4. Docs + review-bot coverage
- [x] docs/coding-standards.md "Suppressions carry checkable evidence":
      "the repo standards say so" is not adequate justification — cite the
      standard AND name this site's verifiable exemption.
- [x] Review bot: the instruction landed upstream — omp-config PR #40
      adds the suppression-why bar to `pr_reviewer.extra_instructions`
      and the same "Suppressions carry checkable evidence" section to
      the canonical standards/coding-standards.md every repo seeds from.
- [x] `bulk-suppression` message points at `[lucidlint.guidance]`.

### 5. Ship
- [x] Full battery: `cargo test`, `pytest` (143), `make check`, `make
      rules` idempotent, self-check PASS, coverage refreshed.
- [x] Commit + push the branch; PR #5 re-reviews the new commits
      (messages, bulk warning, header, guidance).
- [ ] After merge/release (v0.3.1 still needs user approval), re-run on
      houses: the new wording + bulk warning + guidance should retire
      the mass-why pattern; the ~26 suppressions obsoleted by the
      detached-method/positional-literals fixes drop out.

## Anti-goals (recorded so nobody re-opens them)

- **No fixer-side refusal of value-spelled names** (D) — user declined;
  a `SECONDS_PER_MINUTE` may legitimately be the domain noun when the
  repo uses raw units; only the MESSAGE steers (A).
- **No blanket config ignores as the answer** — guidance (B) informs;
  suppression (config `ignore`) remains per-site with whys.
- **No enforcement of bulk-suppression** — it is a warn finding:
  visibility, not a hard gate.

## Acceptance criteria

1. An agent reading a magic-number finding sees "domain noun, never the
   value" and stops producing `SIXTY`-style renames.
2. A repo with 10+ identical whys for one kind sees a
   `bulk-suppression` warning naming the policy choice.
3. A repo that defines `[lucidlint.guidance]` gets the house rule in
   every affected finding message.
4. Every CLI run (text and `--json`) carries the readable/maintainable/
   obviously-correct header; the LSP never does.
5. The review bot flags suppression whys that merely echo the rule or
   cite "the standards" without site-specific evidence.

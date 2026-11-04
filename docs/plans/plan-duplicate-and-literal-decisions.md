# Decisions — the duplicate near-rule and repeated-literal

Two proposals from the open issue set were gated on measurement before any
rule code. Both measurements are below; neither shipped.

## Duplicate near-rule (#62) — declined

### What was wrong

`duplicate` was already repo-wide and fired at 0.9. The reported silence
came from root inference: `repo_root_impl` took the longest common prefix
of the passed paths, so a single-package layout (`pkg/a.py` + `pkg/b.py`)
produced root `.../pkg`, repo-wide findings were reported as `a.py`, and
the orchestrator dropped them silently. Fixed in the root-resolution PR,
which also makes an unmappable finding file a visible diagnostic.

### Measurement

Threshold patched to 0.7 and run on the two evaluation repos, then at the
shipped 0.9:

| repo | findings @0.9 | findings @0.7 |
|---|---|---|
| houses | 7 | 7 |
| family-history-album | 24 | 24 |

No finding lives in the 0.7–0.9 band on either corpus, so a
`duplicate-near` kind would add a rule, a message and a false-positive
surface for zero findings. The motivating one-line normaliser is also
excluded by `is_duplicate_candidate` (>= 2 statements), which a threshold
change does not touch.

### Decision

No `duplicate-near`, no threshold change. #62 closes as fixed by the root
fix; the near-duplicate idea needs a corpus that shows a gap first.

## Repeated literal (#60) — deferred

### Measurement

The proposal's own filter (string constants in path/format construction,
>= 3 uses across >= 2 modules) on the same two repos:

| repo | literal groups that would fire |
|---|---|
| houses | 161 |
| family-history-album | 169 |

The output is dominated by formatting punctuation — `", "` (78 uses),
`": "` (66), `",.2f"` (66), `" ("` (40), `"  "` (27). The ticket's own
signal (`"ocr-guess"`: 22 uses across 11 modules) is real but buried.

### Decision

Not shipped. The ticket's distinction — vocabulary *derived from the same
data*, not merely repeated — needs the receiver's shape in the key
(same leading receiver or pair), which is a different rule from the one
proposed. The issue stays open with the measurement as the starting
constraint; a rule that fires 160 times per repo is a noise generator.

## Recorded with the same round

- #63 (repo-wide findings vanish in a single-package layout) — fixed; the
  root is passed explicitly (`--root`) and an unmappable finding rel is
  logged, not swallowed.
- #64 (`flat-folder`) — shipped as a warn rule at 20+ browsable files per
  directory, counting source modules and docs, excluding `__init__.py`,
  test paths (unless `--include-tests`) and vendored trees.
- #52 (class-module on a DI seam) — the exemption now covers a
  declarations-only `Protocol`/`ABC` every of whose members the namesake
  class implements, with no other implementer in the module.
- #61 (`sibling-value-class`) — shipped as a warn rule; package-wide, with
  containment, inheritance and signature mentions excluded.

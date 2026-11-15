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

## Repeated literal (#60) — shipped as `shared-path` (warn)

### The measurement that rejected the literal-only filter

The proposal's own filter (string constants in path/format construction,
>= 3 uses across >= 2 modules) on the two repos:

| repo | literal groups that would fire |
|---|---|
| houses | 161 |
| family-history-album | 169 |

The output is dominated by formatting punctuation — `", "` (78 uses),
`": "` (66), `",.2f"` (66), `" ("` (40), `"  "` (27). The ticket's own
signal (`"ocr-guess"`: 22 uses across 11 modules) is real but buried.

### What shipped

`shared-path` (warn, Python) is a repo-wide pass over the outermost path
expressions of module-level functions. It groups them by *shared runs* of
segments — a name, a quoted literal, a `Path()` constructor, an f-string
shape — and reports one finding per run.

- A run qualifies when it contains a path-shaped domain literal and appears
  in >= 2 production modules; a single literal needs >= 3 uses.
- A suffix written as a literal (`.layout.json`) and as an f-string tail
  (`f"{page}.layout.json"`) normalise to the same run.
- The message names the pair and asks whether it and the run are one
  concept; otherwise it says the path or name needs one home.
- Test files, nested functions, bare extensions (`.txt`) and format
  punctuation do not count.

### Two-repo measurement

| repo | findings |
|---|---|
| houses | 3 |
| family-history-album | 5 |

Examples: `Path() / "data"` (8 modules), `"words.json"` (7),
`"strokes.json"` (4), `".layout.json"` (2), `"index.html"` (3),
`out / "report.md"` (2).

### Decision

The literal-only filter was noise; the run plus path-shape gate is the
shipped signal. The general duplicate-expression case is #104.

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

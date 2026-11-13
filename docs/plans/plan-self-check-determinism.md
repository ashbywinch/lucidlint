# Plan — the gate tells the truth on every machine

The same commit passed `make self-check` in CI and failed on a developer
machine with 27 findings. CI's fresh clone has no
`.code-review-graph/graph.db`; the machine has one, so the optional graph
families fired. The verdict must not depend on a machine-local artifact, and
the families that fired had to be judged one by one: 25 of the 27 were the
tool misreporting its own documented behaviour, 2 were true.

## Root causes and decisions

| Family | Reported | The defect | The fix |
|---|---|---|---|
| `churn-untested` (23) | docs, covered files, files absent from the snapshot | used the graph's `TESTED_BY` approximation, ignored the repo's own coverage snapshot, and treated any churny file as testable | R6/D6: the repo's coverage snapshot decides; only code files are in the population; a file the snapshot does not name is UNKNOWN; a stale snapshot reports `verify` as a warning. The orchestrator hands the snapshot to the scan core (`--coverage`). |
| `hub-file` (2) | `lucidlint.py`, `fix_engine.py` | counted outgoing call/import edges in `.py` files — a size/density proxy, not the "central module" the rule's row claims; incoming edges were never counted and Rust was invisible | The fact is coupling (fan-in): distinct modules that import the file or call into symbols it declares. Tests and package facades are excluded; tests-only dependents do not count without `--include-tests`. The message names the dependents. Threshold 15 (measured below). |
| `large-function` (2) | `fix_split_module` 372 lines, `fix_collapse_chain` 176 | true, but Python spans came only from graph nodes, so CI and the LSP were blind to a 372-line function | The span is computed in the per-file scan (the file's own AST); the graph duplicate is deleted. The two functions are split along the phases their own code names (locate/resolve/refuse/rewrite; plan/cluster/layout/refuse/build/verify), not into equal chunks. |
| suppressed findings | `large-function` vanished under an unrelated marker | a marker exempted *every* finding at the same line/col when its signal matched any one of them, so `ignore closures` silenced a `large-function` | A marker binds only the signals it names; one comma-named line (`ignore closures,large-function <why>`) covers several. Stacked markers were never in the documented window — the repo's own pairs were merged into comma lists. |
| `special-case` (2 new) | `found`, `callee` | the counter summed the same local name across unrelated functions (a file-wide count) | Count within one scope; nested function/class bodies are scopes of their own. |

## Measured hub-file threshold

Distinct dependent modules per file (tests excluded), before choosing 15:

| repo | top dependents |
|---|---|
| houses | 51 `dag/attempt.py`, 46 `serialization/__init__.py`, 42, 40, 38 … |
| family-history-album | 28 `tools/records.py`, 22 `loft/live/errors.py`, 19, 16 |
| chat-workflow | 17 `chat_workflow/__init__.py`, 11 |
| books_to_anki | 8 |
| lucidlint itself | 2 |

15 keeps the rule on modules whose change reaches 15+ dependents and off
cohesive large leaf modules — `fix_engine.py` has one production dependent.

## Verification

- `make test`, `make check`, `make self-check` green.
- A CI-shaped copy of the tree (with `.git`, without `.code-review-graph`
  and without coverage data) reports `GATE: PASS` with the same warnings.
- `RULES.md` regenerated (`make rules`); `churn-untested`, `hub-file` and
  `large-function` rows state the behaviour above.
- house/album re-runs: `hub-file` now names the dependent modules (houses:
  8 modules, all imported by 15–51 others) instead of flagging every ~1k-line
  file; `large-function` fires on long functions without the graph.

# lucidlint — deterministic code health for humans and agents

Lucidlint is a code-health gate for Python and Rust that tells you — and
your coding agent — *exactly* what to fix and how, with zero guesswork.
It scans for architecture-level problems: complexity, duplicate code,
swallowed errors, test quality, layering. Same code, same report, every
run — there is no model judgment anywhere in the scan, and every fix it
offers is a deterministic refactoring the tool can apply itself.

Built for the era when much of the code being written is written by
agents. A lucid codebase is one an agent — or you, six months later — can
modify safely, because the invariants are visible and every exception is
documented.

```
$ lucidlint --repo .
GATE: FAIL — 2 action(s) ... top P99 houses/app.py:149 (parse_netex_fares)
  [complexity] houses/app.py:149 — cyclomatic complexity 88 (>= 15) — fix: lucidlint fix --kind extract-method --file houses/app.py --line 149
  [swallow]    houses/app.py:210 — except that swallows — re-raise or handle it
```

---

## Decide if this is what you need

**You want lucidlint if you write (or generate) Python or Rust and care
that the code stays explainable:**

- your functions stay under ~15 decision points and fit on a screen — and
  complexity is *split*, not hidden in a helper that is just as big;
- every non-trivial number is a named constant, not a magic literal;
- every error is handled, re-raised, or explicitly surfaced — never
  swallowed;
- a test can actually fail — it has an assertion, doesn't skip on the
  environment, doesn't touch the real filesystem;
- types and classes are named for the domain they model, not their role;
- nothing is suppressed without a written reason — and suppressions that
  stop firing are deleted.

**Why it matters for agent-created code.** Agents write code fast and in
volume, and two failure modes compound in their output: *latent
complexity* (each individual change looks reasonable; the function creeps
past the budget one commit at a time) and *borrowed patterns* (an agent
copying a shape from another file propagates its problems). Unlucid code
is where agents confidently make the wrong change, because the hidden
state and swallowed errors are invisible to them too.

**What it is not:**

- not a formatter or a style linter — it finds *structural* problems with
  real consequences (a swallowed error, a function past its complexity
  budget, a test that can never fail);
- not a code reviewer — it makes no judgment calls, ever; the same input
  always produces the same report;
- not a model — the scan is a compiled binary. Deterministic checks beat
  model judgment.

**What it finds** — the full, generated rule reference is in
[RULES.md](RULES.md). A quick map:

| Area | Examples |
|---|---|
| Correctness | swallowed errors, debug artifacts (`dbg!`, `breakpoint`, `.unwrap()`), dead statements, unreachable code, boolean-literal args, shadowed builtins |
| Complexity | cyclomatic CC ≥ 15, functions ≥ 120 lines, > 5 parameters, near-duplicate code |
| Architecture | import cycles, layer violations, record-shaped structs, strewing, latent classes, churn without tests |
| Tests | monkeypatch, skipped tests, real filesystem I/O, tests with no assertion |
| Suppressions | every `ignore`/`allow`/`noqa` needs a written reason — and stale ones are deleted |

## Designed for agents

The tool is built around how agents actually work — nothing about using it
burns tokens:

- **They always understand what they should change and why.** Every
  finding names the rule, the evidence (the exact number, function,
  line), and the full command to run. The message ends with a
  machine-parseable `fix:` directive.
- **They learn about a problem as soon as they create it.** Run as an LSP
  (see below), the tool checks every file on save — the finding appears
  the moment the complexity creeps past the threshold, not at review time
  when the context is gone.
- **They spend minimum tokens fixing it.** The fix surface is one command
  in, one diff out: the tool previews the exact seam it will move, the
  agent supplies the one thing the tool cannot invent — a name — and the
  refactoring lands, verified. No trial-and-error, no counting lines (the
  tool owns its coordinates), no re-scanning to see if it worked.

### The agent loop, end to end

```
1. The gate (or LSP on save) reports a finding:
   [complexity] houses/app.py:149 (parse_netex_fares)
   cyclomatic complexity 88 (>= 15) — fix: lucidlint fix --kind extract-method
       --file houses/app.py --line 149

2. The agent runs the message's command — no name needed yet:
   lucidlint fix --kind extract-method --file houses/app.py --line 149

3. The tool shows the seam as a preview — what moves, its new signature,
   its first lines, and the exact command to apply:
   ...
   -    for dmep in root.iter():
   ...
   +    _extracted(dme_zone_pairs, root, zone_fares)
   # seam: line 305: for dmep in root.iter():

   Extracted (the method being created, first lines):

   def _extracted(dme_zone_pairs, root, zone_fares):
       for dmep in root.iter():
   ...
   # the name `_extracted` is a placeholder — pick a real one; apply it:
   #   lucidlint fix --kind extract-method --file houses/app.py --line 149 \
   #     --name lookup_dmep_prices

4. The agent supplies the semantic bit — the name — and applies:
   lucidlint fix --kind extract-method --file houses/app.py \
       --line 149 --name lookup_dmep_prices

5. The extraction lands: a private helper (the underscore is automatic —
   a fresh extraction has no external callers), the original function
   drops under the gate, and the next run is clean. The tool verifies its
   own work: if the seam can't actually split the complexity, it refuses
   rather than proposing a broken refactoring.
```

---

## Install

**Requirements:** Python ≥ 3.12, 64-bit Linux / macOS / Windows. The scan
engine ships as a compiled binary — the bundle is self-contained, no
`cargo`, no `PATH` fiddling.

### Option 1 — the release bundle (recommended, self-contained)

Download the archive for your platform from the
[releases page](https://github.com/ashbywinch/lucidlint/releases)
(`SHA256SUMS` is published alongside for verification):

```
linux x64   lucidlint-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz
macOS arm64 lucidlint-vX.Y.Z-aarch64-apple-darwin.tar.gz
macOS x64   lucidlint-vX.Y.Z-x86_64-apple-darwin.tar.gz
Windows x64 lucidlint-vX.Y.Z-x86_64-pc-windows-msvc.tar.gz
```

```bash
tar xzf lucidlint-vX.Y.Z-<platform>.tar.gz
cd lucidlint-vX.Y.Z-<platform>/
./bin/lucidlint --version        # "lucidlint vX.Y.Z"
python3 lucidlint.py --repo .    # GATE: PASS / FAIL
```

The orchestrator finds its sibling `bin/lucidlint` by itself — no PATH, no
make, no cargo.

### Option 2 — pip (the `lucidlint` command)

```bash
pip install lucidlint            # once published to PyPI
# or directly from GitHub today:
pip install "git+https://github.com/ashbywinch/lucidlint.git"
lucidlint --repo .
```

The pip install gives the `lucidlint` command (no `.py`, no flags with
`fix-` prefixes). The wheel is self-contained: setup.py compiles the Rust
scan core INTO the wheel, so scan, fix, and the LSP all work with no
release bundle, no PATH setup, and no `make`.

### Option 3 — as an LSP (checks what you type, on save)

The same binary is a language server. Point your editor's LSP client at:

```
lucidlint --lsp
```

The binary speaks stdio JSON-RPC and checks `.py` and `.rs` files — each
`didOpen`/`didChange`/`didSave` runs the per-file scan in process and
returns diagnostics, so you (or your agent) see findings the moment they
are created. Editor recipes (VS Code, Neovim, Emacs/eglot, Helix) are
[in this repo's docs](docs/lsp-setup.md).

### Option 4 — from a script (git hooks, CI)

The gate is a plain CLI — call it anywhere a shell runs:

```bash
# git pre-commit hook
python3 lucidlint.py --repo . --baseline lucidlint.json || exit 1
```

```yaml
# CI (GitHub Actions — the exit code is the verdict; other CI: same command)
- name: lucidlint gate
  run: python3 lucidlint.py --repo . --baseline lucidlint.json
```

`--json` emits the full action model (kind, severity, file, line,
function, message, metric, churn, priority) for other tooling. The scan
never needs a network or a remote — it reads the working tree.

### First run and the baseline

```bash
lucidlint --repo .                 # see today's debt
lucidlint --repo . --update-baseline --baseline lucidlint.json   # acknowledge it
lucidlint --repo . --baseline lucidlint.json   # now fails only on NEW findings
```

Baselines lock today's debt so the gate blocks only what is new — and a
baseline entry whose finding disappeared is itself a FAIL (your debt
shrinks, the baseline shrinks with it).

---

## Use

### The gate

`lucidlint --repo .` prints a verdict and the prioritized findings, and
exits:

| Exit | Meaning |
|---|---|
| 0 | PASS — clean, or everything is baselined; warnings (magic numbers, broad excepts) are reported but never block |
| 1 | FAIL — new fail-severity findings, or the baseline is stale |
| 2 | usage or configuration error |

### The fix surface

Every fixable finding's message ends with the full command — copy it, run
it. The `fix` subcommand:

```
lucidlint fix --kind <family> --file <file> [--line <line>] [--name <name>] [--params a,b] [--confirm]
```

- **Mechanical** families (stale-suppression, noop-statement, unreachable,
  positional-literals) apply directly — the tool edits the one node,
  losslessly, and the next run confirms the finding is gone.
- **Structural** families (extract-method, extract-class, long-param-list,
  magic-number, vague-name) preview first — the tool shows the diff, the
  seam, the new signature, and the exact apply command; the name IS the
  commitment. `--line` is optional when the file has exactly one finding
  of the kind (R27: the tool owns its coordinates — agents never count
  lines).

### Configuration

A `.lucidlint.toml` (or `[tool.lucidlint]` in `pyproject.toml`) silences
rules or whole groups repo-wide, with per-path overrides:

```toml
[lucidlint]
ignore = ["vague-name"]
[lucidlint."tests/**"]
ignore = ["group:architecture"]
```

Every rule is individually suppressible — a rule that doesn't fit your
project is acknowledged debt, not a blocker.

---

## Contribute

Everything is deterministic and tested, and the tool gates its own repo —
`make self-check` must pass before a change lands (the house code is the
exemplar of every rule it enforces).

**Repo layout**

| Path | What it is |
|---|---|
| `scanner/` | the Rust scan core: every finding family + the radon-exact CC (`radonc/`), the LSP, the CLI scan |
| `lucidlint.py` | the orchestrator: file gathering, actions, ranking, baselines, the gate verdict, the `fix` subcommand |
| `fix_engine.py` | the deterministic refactorings (libcst): extract-method, extract-class, parameter objects, renames |
| `rule_metadata.py` | the canonical rule registry — RULES.md is generated from it (`make rules`) |
| `docs/` | PRD (requirements), TECHSPEC (mechanics), PLAN (phases), the standards |

**Getting started**

```bash
make setup          # symlink hooks etc.
make test           # lint + typecheck + scanner tests + pytest
make self-check     # the tool gates itself — must be GATE: PASS
make rules          # regenerate RULES.md from rule_metadata.py
```

**Adding a finding family** — follow the checklist in
[RULES.md](RULES.md) ("Adding a finding family"): emit it in the scanner,
register it in `FAMILY_KINDS` + `RULE_GROUPS` + `rule_metadata.py`,
add the `fix:` directive, test it, and keep `make self-check` green.

**How it works** — the requirements live in [docs/PRD.md](docs/PRD.md),
the mechanics in [docs/TECHSPEC.md](docs/TECHSPEC.md), the delivery plan
in [docs/PLAN.md](docs/PLAN.md).

---

## Documentation index

- **[RULES.md](RULES.md)** — every rule, severity, language, and what it
  checks (generated from `rule_metadata.py`).
- **[docs/PRD.md](docs/PRD.md)** — the product requirements (R1–R27): why
  each capability exists.
- **[docs/TECHSPEC.md](docs/TECHSPEC.md)** — how it's built: components,
  object model, architecture, technology choices, strategic decisions.
- **[docs/PLAN.md](docs/PLAN.md)** — the delivery phases.
- **[docs/lsp-setup.md](docs/lsp-setup.md)** — editor-by-editor LSP
  configuration.
- **[docs/coding-standards.md](docs/coding-standards.md)** ·
  **[docs/testing-standards.md](docs/testing-standards.md)** ·
  **[docs/ux-standards.md](docs/ux-standards.md)** — the house standards
  the tool enforces (and the ones scaffolded into new repos).
- **[docs/writing-documentation.md](docs/writing-documentation.md)** ·
  **[docs/documentation-structure.md](docs/documentation-structure.md)** —
  the documentation standards.

## License

MIT. Issues and PRs at [github.com/ashbywinch/lucidlint](https://github.com/ashbywinch/lucidlint).

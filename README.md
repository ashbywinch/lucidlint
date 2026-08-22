# lucidlint — deterministic Python maintainability for agents

Lucidlint is a code maintainability gate for Python, intended to assist agents in writing highly readable, well structured code that is visibly correct and anchored in the user's domain. It sits a level up from a linter like Ruff or type checker like Pyrefly, looking at design concerns at the function or class level, rather than line by line stylistic issues.

Lucidlint packages fast deterministic auto fixes for many of its errors, reducing token spend on refactoring and rework. Agents can be quite bad at keeping track of line numbers to do repeated surgical edits - Lucidlint gets it right the first time, every time, fast and without spending tokens.

Lucidlint is highly opinionated, and produces errors for many things that human developers might consider stylistic choices. We find that human and agents alike do better work in less complex code that makes heavy use of nouns and verbs from the domain as variable, function and class names. In a codebase predominantly consumed by agents, it's reasonable and effective to be extremely strict about complexity minimisation and about separation of concerns, forcing agents to introduce more domain nouns and verbs as names in the code. This makes the code much more readable and in turn exposes bugs and makes changes less risky.

Lucidlint enforces that all rule exceptions (including exceptions for other lint or type products) have a reason provided. This allows you to check during code review that the provided reason is adequate and not covering up problems.

Lucidlint can run as an [LSP](https://microsoft.github.io/language-server-protocol/overviews/lsp/overview/), so that agents keep their work tidy as they go. It can process a single file in well under a second and an entire medium sized repo in three to four seconds.

Lucidlint is **not**:

- An LLM or AI agent. All findings and fixes are 100% deterministic.
- A style linter or type checker.

```
$ lucidlint --repo .
GATE: FAIL — 2 action(s) ... top P99 houses/app.py:149 (parse_netex_fares)
  [complexity] houses/app.py:149 — cyclomatic complexity 88 (>= 15) — fix: lucidlint fix --kind extract-method --file houses/app.py --line 149
  [swallow]    houses/app.py:210 — except that swallows — re-raise or handle it
```

---

**What it finds** — the generated rule reference is in
[RULES.md](RULES.md). 

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
   drops under the gate, and the next run is clean. 
```

---

## Install

**Requirements:** Python ≥ 3.12, 64-bit Linux / macOS / Windows. 

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

### Option 2 — pip (the `lucidlint` command)

```bash
pip install lucidlint            # once published to PyPI
# or directly from GitHub today:
pip install "git+https://github.com/ashbywinch/lucidlint.git"
# or from a downloaded artifact (the releases page, a CI copy):
pip install ./lucidlint-0.2.0-py3-none-linux_x86_64.whl
lucidlint --repo .
```

The pip install gives the `lucidlint` command (no `.py`, no flags with
`fix-` prefixes). 
The platform-independent sdist (`lucidlint-X.Y.Z.tar.gz`) installs anywhere, but it builds the Rust core at install time, so it needs `cargo`.

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
function, message, metric, churn, priority) for other tooling.

### First run and a baseline

```bash
lucidlint --repo .                 # see today's debt
lucidlint --repo . --update-baseline --baseline lucidlint.json   # acknowledge it
lucidlint --repo . --baseline lucidlint.json   # now fails only on NEW findings
```

Baselines lock today's debt so the gate blocks only what is new. Lucidlint will raise an error if the baseline can be lowered.

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

### Configuration

A `.lucidlint.toml` (or `[tool.lucidlint]` in `pyproject.toml`) silences
rules or whole groups repo-wide, with per-path overrides:

```toml
[lucidlint]
ignore = ["vague-name"]
[lucidlint."tests/**"]
ignore = ["group:architecture"]
```

Every rule is individually suppressible.

---

## Contribute

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


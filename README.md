# lucidlint — deterministic code health for humans and agents

Lucidlint is a deterministic code-health gate for Python and Rust. It scans
your codebase for architecture-level problems — complexity, duplicate code,
swallowed errors, test quality, layering — and produces a single
`GATE: PASS` / `GATE: FAIL` verdict. Same code, same report, every run:
there is no model judgment anywhere in the scan.

## What "lucid code" means

Lucid code is code a reader — human or agent — can look at and immediately
understand *why it exists and what it does*. Concretely, lucidlint's rules
define it:

- every non-trivial number is a named constant, not a magic literal;
- every error is either handled, re-raised, or explicitly surfaced —
  never swallowed;
- a function has at most ~15 decision points and fits on a screen — and
  the complexity is *split*, not hidden in a helper that is just as big;
- a test can actually fail — it has an assertion, doesn't skip on the
  environment, doesn't touch the real filesystem;
- types and classes are named for the domain they model, not their role;
- nothing is suppressed without a written reason — and suppressions that
  stop firing are deleted.

**Why you want it, especially in agent-created code:** agents write code
fast and in volume, and two failure modes compound in their output —
*latent complexity* (each individual change looks reasonable; the function
creeps past the complexity budget one commit at a time) and *borrowed
patterns* (an agent copying a shape from another file propagates its
problems). A lucid codebase is one where a future agent — or you, six
months later — can modify any part safely, because the invariants are
visible and the exceptions are documented. Unlucid code is where agents
confidently make the wrong change, because the hidden state and swallowed
errors are invisible to them too.

## Designed for agents

The tool is built around how agents actually work:

- **They always understand what they should change and why.** Every
  finding names the rule, the evidence (the exact number, function, line),
  and — where a fix exists — the exact command to run. The message ends
  with a machine-parseable `fix:` directive.
- **They learn about a problem as soon as they create it.** Run as an LSP,
  the tool checks every file on save. The agent sees the finding the moment
  the complexity creeps past the threshold — not at review time, when the
  context is gone.
- **They spend minimum tokens fixing it.** Deterministic fixes mean no
  exploration: the agent runs one command, reviews a precise diff, and
  applies. No trial-and-error, no guessing line numbers (the tool owns its
  coordinates), no re-scanning to see if the fix worked. Where a
  refactoring needs a judgment call — a name — the tool shows the seam and
  asks for just that one thing.

None of this burns tokens: the scan is a compiled binary, the findings are
one line each, and the fix flow is one command in, one diff out.

### The agent loop, end to end

```
1. Gate (or LSP on save) reports a finding:
   [complexity] houses/app.py:149 (parse_netex_fares)
   cyclomatic complexity 88 (>= 15) — fix: extract-method

2. Agent runs the fix command — no name needed yet:
   lucidlint.py --repo . --fix-kind extract-method \
       --fix-file houses/app.py --fix-line 149

3. The tool shows the seam as a preview — what moves, its signature,
   its first lines, and the exact command to apply:
   ...
   -    for dmep in root.iter():
   -        tag = _unprefixed(dmep.tag)
   ...
   +    _extracted(dme_zone_pairs, root, zone_fares)
   # seam: line 305: for dmep in root.iter():

   Extracted (the method being created, first lines):

   def _extracted(dme_zone_pairs, root, zone_fares):
       for dmep in root.iter():
           tag = _unprefixed(dmep.tag)
   ...
   # the name `_extracted` is a placeholder — pick a real one; apply it:
   #   --fix-kind extract-method --fix-file houses/app.py --fix-line 149 \
   #   --fix-name <name>

4. The agent supplies the semantic bit — the name — and applies:
   lucidlint.py --repo . --fix-kind extract-method \
       --fix-file houses/app.py --fix-line 149 --fix-name lookup_dmep_prices

5. The extraction lands: a private helper (the underscore is applied
   automatically — a fresh extraction has no external callers), the
   original function drops under the complexity gate, and the next gate
   run is clean. The tool verifies its own work: if the seam can't
   actually split the complexity, it refuses rather than proposing a
   broken refactoring.
```

## Quick start

```bash
# 1. Download the latest release bundle
#    (https://github.com/ashbywinch/build-tools/releases)
#    Choose your platform: linux musl, macos arm64/x64, windows x64

tar xzf lucidlint-v0.1.0-<platform>.tar.gz
cd lucidlint-v0.1.0-<platform>/

# 2. Run the gate on a Python or Rust project
./bin/lucidlint --version        # "lucidlint v0.1.0"
python3 lucidlint.py --repo .  # GATE: PASS / FAIL

# 3. (optional) Acknowledge today's debt — the gate then fails only on NEW findings
python3 lucidlint.py --repo . --update-baseline --baseline lucidlint.json
```

The bundle is self-contained: `lucidlint.py` finds its sibling `bin/lucidlint` by itself — no `PATH`, no `make`, no `cargo` needed.

## What it finds

[**Full rule reference → RULES.md**](RULES.md)

| Group | What the gate flags |
|---|---|
| **Style & correctness** | Magic numbers, dead statements, unreachable code, `dbg!()`/`.unwrap()`/`breakpoint()` left in, boolean-literal arguments, shadowed builtins, broad excepts, swallowed errors, inline/private imports, unused functions, circular imports, broken doc links |
| **Complexity & size** | Cyclomatic CC ≥ 15, functions ≥ 120 lines, > 5 parameters, near-duplicate code |
| **Architecture** | Import cycles, layer violations, folder community splits, hub files, high-risk untested functions, record-shaped structs, strewing, latent classes, abstract classes with one subclass, churn without tests |
| **Test discipline** | `monkeypatch` / `patch` instead of DI, `skipif`/`#[ignore]`/permanent skips, real filesystem I/O without `pyfakefs`, tests with no assertion |
| **Suppressions** | Any `ignore` / `allow` / `type: ignore` / `noqa` without a written reason — plus suppressions that no longer fire (stale) |
| **Refactoring advice** | Guard clauses, latent visitors, conditional polymorphism, special cases, middle men, unused setters, loop pipelines — detected, never blocked |
| **&dagger; Controversial** | Vague role names (Manager/Handler), closures as missed classes, inline imports for performance, swallowed errors, global state, file churn × CC hotspots, community-based folder mixing, similarity-based duplicates |

&dagger; These rules can conflict with existing team conventions or framework idioms. Each is individually suppressible (see RULES.md). A rule that doesn't fit your project is acknowledged debt, not a blocker.

## Verdict

```
GATE: PASS — 0 action(s) acknowledged in baseline (29 warnings reported, never fail)
```

The gate exits:

| Exit | Meaning |
|---|---|
| **0** | **PASS** — clean, or all findings are acknowledged in the baseline; only warnings (magic numbers, broad excepts) remain |
| **1** | **FAIL** — new fail-severity findings exist, or the baseline contains entries the code no longer produces (stale baseline — run `--update-baseline`) |
| **2** | Usage or configuration error |

- **Warn tier**: Findings like magic numbers are reported but never cause a FAIL. They're visible in the output so you can fix them if you want, but they don't block merges.
- **Baselines**: `--update-baseline --baseline lucidlint.json` captures the current state of fail-severity findings. Subsequent runs only fail on NEW findings. A stale baseline (entries that no longer correspond to any code) is itself a FAIL — your debt shrinks, and the baseline shrinks with it.

## Using lucidlint from a script

The gate is a plain CLI — call it anywhere a shell runs:

**Git pre-commit hook** (`.git/hooks/pre-commit`, or via `pre-commit`):

```bash
#!/bin/sh
# fail the commit on new findings; acknowledge known debt in lucidlint.json
python3 /path/to/lucidlint.py --repo . --baseline lucidlint.json || exit 1
```

**CI** (any runner — the exit code is the verdict):

```yaml
# GitHub Actions
- name: lucidlint gate
  run: python3 lucidlint.py --repo . --baseline lucidlint.json
# other CI: run the same command; exit 1 blocks the pipeline
```

**A full scan with JSON output** (for other tooling):

```bash
python3 lucidlint.py --repo . --json > lucidlint-report.json
```

The JSON has the full action model: kind, severity, file, line, function,
message, metric, churn, priority. The scan itself never needs a git remote
or network — it reads the working tree and (optionally) the
`code-review-graph` contract and `coverage.xml` if you have them.

## Installation as an LSP

The same binary serves as a language server — it checks what you type, in
your editor, on save. Each LSP method (`didOpen`, `didChange`, `didSave`)
runs the per-file scan *in process* (no shell, no spawn) and returns
diagnostics.

### Generic LSP configuration

Point your editor's LSP client at:

```
lucidlint --lsp
```

The binary speaks stdio JSON-RPC (Content-Length framing). Any editor with
LSP support can use it.

### Editor-specific notes

**VS Code**: Create a `.vscode/tasks.json` or a custom extension that starts the server. Alternatively, use the "LSP Client" extension (or your preferred one) with the command `lucidlint --lsp`.

**Neovim** (built-in LSP):
```lua
vim.api.nvim_create_autocmd({ "BufEnter" }, {
  pattern = { "*.py", "*.rs" },
  callback = function()
    vim.lsp.start({
      name = "lucidlint",
      cmd = { "/path/to/lucidlint", "--lsp" },
    })
  end,
})
```

**Emacs** (eglot):
```elisp
(add-to-list 'eglot-server-programs
  '((python-mode rust-mode) . ("/path/to/lucidlint" "--lsp")))
```

**Helix** (built-in LSP — add to your `languages.toml`):
```toml
[language-server.lucidlint]
command = "/path/to/lucidlint"
args = ["--lsp"]

[[language]]
name = "python"
language-servers = ["lucidlint"]

[[language]]
name = "rust"
language-servers = ["lucidlint"]
```

## Auto-fixing findings

Every fixable finding's message ends with a machine-parseable `fix:`
directive — the agent (or you) runs it and the tool does the rest. The
flow differs by fix family:

- **Mechanical** (stale-suppression, noop-statement, unreachable,
  positional-literals): apply directly — the tool edits the one node,
  losslessly (libcst), and the gate re-run confirms the finding is gone.
- **Structural** (extract-method, extract-class, long-param-list,
  magic-number, vague-name): the tool previews first. No name needed to
  see the seam; the preview shows the diff, the seam anchor, the new
  signature, and the exact apply command. Then one tool call with the
  name applies it — the name IS the commitment, no `--confirm` dance.

```bash
# preview a complexity extraction (no name needed):
lucidlint.py --repo . --fix-kind extract-method --fix-file x.py --fix-line 149
# apply it, naming the extracted method:
lucidlint.py --repo . --fix-kind extract-method --fix-file x.py \
    --fix-line 149 --fix-name lookup_dmep_prices
# mechanical fix, direct:
lucidlint.py --repo . --fix-kind stale-suppression --fix-file x.py --fix-line 12
```

The tool guarantees the refactoring is correct: seams are self-contained
(no out-variables, no control flow moved), bounded to split the complexity
on both sides, and verified by behavior-preservation tests. When no safe
seam exists it says so — it never writes a broken file.

## Configuration

Create a `.lucidlint.toml` in your repo root (or add a `[tool.lucidlint]` section to `pyproject.toml`) to suppress entire rules or rule groups across your codebase:

```toml
# Suppress specific rules
[lucidlint]
ignore = ["vague-name", "inline-import"]

# Suppress whole groups (definitions in RULES.md)
# ignore = ["group:architecture", "group:test-discipline"]

# Per-path overrides — silence rules only in certain paths
[lucidlint."tests/**"]
ignore = ["group:architecture"]

[lucidlint."scripts/**"]
ignore = ["global-state"]
```

The config works at the orchestrator level — no recompile needed, no per-file suppression comments. The same rule references as the suppression system (`ignore` + `group:` prefix) with the same semantics: every rule can be silenced, and a config change is visible in the repo's diff.

## Project status

Lucidlint is in active development toward a v0.8 public release. The version scheme is 0.x — minor bumps per capability (release pipeline, fleet wiring, PyPI wheels, the Rust library split). The rule set grows with each minor version; baselines absorb new findings so existing projects stay green.

## License & contributing

MIT. Issues and PRs at [github.com/ashbywinch/build-tools](https://github.com/ashbywinch/build-tools).

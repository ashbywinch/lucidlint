# lucidlint — your codebase's sanity check

A deterministic code-health gate for Python and Rust. It scans architecture-level problems — complexity, duplicate code, import cycles, test quality, layering — and produces a single `GATE: PASS` / `GATE: FAIL` verdict. No false-positive drift: same code, same report, every run.

## Quick start

```bash
# 1. Download the latest release bundle
#    (https://github.com/ashbywinch/build-tools/releases)
#    Choose your platform: linux musl, macos arm64/x64, windows x64

tar xzf lucidlint-v0.1.0-<platform>.tar.gz
cd lucidlint-v0.1.0-<platform>/

# 2. Run the gate on a Python or Rust project
./bin/lucidlint --version        # "lucidlint v0.1.0"
python3 code_health.py --repo .  # GATE: PASS / FAIL

# 3. (optional) Acknowledge today's debt — the gate then fails only on NEW findings
python3 code_health.py --repo . --update-baseline --baseline code-health.json
```

The bundle is self-contained: `code_health.py` finds its sibling `bin/lucidlint` by itself — no `PATH`, no `make`, no `cargo` needed.

## What it finds

[**Full rule reference → RULES.md**](RULES.md)

| Group | What the gate flags |
|---|---|
| **Style & correctness** | Magic numbers, dead statements, unreachable code, `dbg!()`/`.unwrap()`/`breakpoint()` left in, boolean-literal arguments, shadowed builtins, broad excepts, swallowed errors, inline/private imports, unused functions, circular imports, broken doc links |
| **Complexity & size** | Cyclomatic CC ≥ 15, functions ≥ 120 lines, > 5 parameters, near-duplicate code |
| **Architecture** | Import cycles, layer violations, folder community splits, hub files, high-risk untested functions, record-shaped structs, strewing, latent classes, abstract classes with one subclass, churn without tests |
| **Test discipline** | `monkeypatch` / `patch` instead of DI, `skipif`/`#[ignore]`/permanent skips, real filesystem I/O without `pyfakefs`, tests with no assertion |
| **Suppressions** | Any `ignore` / `allow` / `type: ignore` / `noqa` without a written reason — plus suppressions that no longer fire (stale) |
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
- **Baselines**: `--update-baseline --baseline code-health.json` captures the current state of fail-severity findings. Subsequent runs only fail on NEW findings. A stale baseline (entries that no longer correspond to any code) is itself a FAIL — your debt shrinks, and the baseline shrinks with it.

## Installation as an LSP

The same binary serves as a language server — it checks what you type, in your editor, on save. Each LSP method (`didOpen`, `didChange`, `didSave`) runs the per-file scan *in process* (no shell, no spawn) and returns diagnostics.

### Generic LSP configuration

Point your editor's LSP client at:

```
lucidlint --lsp
```

The binary speaks stdio JSON-RPC (Content-Length framing). Any editor with LSP support can use it.

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

## Project status

Lucidlint is in active development toward a v0.8 public release. The version scheme is 0.x — minor bumps per capability (release pipeline, fleet wiring, PyPI wheels, the Rust library split). The rule set grows with each minor version; baselines absorb new findings so existing projects stay green.

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

## License & contributing

MIT. Issues and PRs at [github.com/ashbywinch/build-tools](https://github.com/ashbywinch/build-tools).
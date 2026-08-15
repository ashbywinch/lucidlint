# code-health-scan — the Rust scan core

The deterministic per-file + repo-wide finding engine behind the code-health
gate. `code_health.py` requires it (one invocation per repo; the gate fails
fast with "build it with `make scanner-check`" when it is missing — the
Python engine exists only as the parity-test reference). Every per-file
family and the two repo-wide families (duplicate, unused) compute here at
exact Python parity.

## CLI contract

```
code-health-scan <file.py>...
```

- Reads each file, emits one JSON object on stdout, exit 0.
- `parse_errors` counts unparseable files (error-tolerant by design).
- `findings`: `{file, line, function, kind, severity, message}` —
  severity is `fail` (fails the gate) or `warn` (reported, never fails).
- `cc`: `{file, function, line, cc}` — radon-equivalent cyclomatic complexity.
- Line/line-1 `# code-health: ignore <kind> <why>` and `ignore-file`
  suppressions are applied before output; a why-less suppression is itself
  a finding.

## LSP wiring (why this is trivial)

A language server wraps the binary per file: spawn on `didOpen`/`didChange`,
parse stdout, map each finding to a `Diagnostic` (line − 1, severity:
`warn` → `Warning`, else `Error`, message as-is, `source: "code-health"`).
No repo state, no git dependency, no config; a single file scans in ~0.2 s
including process startup (largest file in build-tools, 4.1 k lines).

The Python wrapper's richer per-file mode adds the Python-only families
(latent-class partition, record-shape) that need repo context:

```
code_health.py --repo <root> --file <rel> --json
```

Findings arrive as `actions` with `line`/`severity`/`kind`/`message` —
the same diagnostic mapping applies.

## Repo-wide mode

Passing the whole repo's `.py` files (including test files — the reference
scan splits prod vs test) computes `duplicate` (Dice on structural
skeletons, ≥ 0.9) and `unused` (defined-but-never-referenced) across files.

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

## LSP server (built in)

`code-health-scan --lsp` is a stdio JSON-RPC language server. Point an
editor at it as a custom server command — no Python, no wrapper:

- `didOpen` / `didChange` (full sync) / `didSave` → the buffer is scanned
  **in-process** (`scan_source`, no subprocess, no disk round-trip) and
  diagnostics are published; `didClose` clears the gutter.
- Every per-file family plus complexity (CC ≥ 15) becomes a `Diagnostic`
  (line − 1, full-line range, severity `warn` → `Warning` else `Error`,
  `source: "code-health"`). The repo-wide families (duplicate, unused)
  are meaningless for one buffer and are dropped.
- Latency: 5–10 ms per `didChange` on typical files, ~70 ms on the
  4,100-line largest file in build-tools.

The Python wrapper's `--file` mode (`code_health.py --repo <root> --file
<rel> --json`) adds the one Python-only family (latent-class partition)
but pays process startup; the LSP server does not include the partition.

## Repo-wide mode

Passing the whole repo's `.py` files (including test files — the reference
scan splits prod vs test) computes `duplicate` (Dice on structural
skeletons, ≥ 0.9) and `unused` (defined-but-never-referenced) across files.

# Setting up lucidlint as an LSP

The lucidlint binary (`bin/lucidlint` in the release bundle, or the
installed `lucidlint` command) is a language server: it checks `.py` and
`.rs` files on open/change/save and returns diagnostics. Each LSP method
runs the per-file scan *in process* — no shell, no spawn.

## Generic configuration

Point any LSP-capable editor at:

```
lucidlint --lsp
```

The binary speaks stdio JSON-RPC (Content-Length framing).

## VS Code

Start the server from a `.vscode/tasks.json` or a custom extension, or use
an LSP-client extension with the command `lucidlint --lsp`.

## Neovim (built-in LSP)

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

## Emacs (eglot)

```elisp
(add-to-list 'eglot-server-programs
  '((python-mode rust-mode) . ("/path/to/lucidlint" "--lsp")))
```

## Helix (built-in LSP — add to `languages.toml`)

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

## The three-LSP standard

The house setup runs three language servers side by side on Python — each
owns one job, and none re-implements another's:

|Server|Owns|
|---|---|
|`ruff`|Lint: undefined names (F821), unused imports, formatting, import sorting|
|`pyrefly`|Type checking — the both-direction baseline lock lives in `scripts/pyrefly-lock.py`|
|`lucidlint`|The gate findings this repo ships (complexity, coupling, docs integrity, ...)|

The undefined-name check is deliberately NOT a lucidlint rule: ruff F821
owns it, and it fires in the editor at the moment of typing. lucidlint does
not duplicate it, so a buffer with an undefined name shows exactly one
diagnostic from the right server — not two competing ones.

## Notes

- macOS: the first run of a downloaded (unsigned) binary needs
  `xattr -d com.apple.quarantine /path/to/lucidlint`.
- The LSP publishes the same findings as the gate, including the
  `fix:` directives in the messages — an agent editing in the editor sees
  the exact command to run for each finding.
- The server answers `textDocument/codeAction`: one `quickfix` per finding
  whose message carries a `fix:` directive. The action's command arguments
  are `[uri, line, argvTokens]` — the tokens run directly as
  `lucidlint fix --kind <tokens...> --file <path> --line <n>`, with
  `--fix-name` already mapped to the parser's `--name`. Fixes that need a
  name the tool cannot invent (`needsName: true`, kinds listed in the
  catalog's `fix_name_required`) carry the message's `<placeholder>` in the
  tokens: prompt for the real name and substitute it before running, or the
  CLI refuses with an explicit message instead of applying garbage. A
  `source.fixAll` action appears when any fixable finding is in range;
  apply its mechanical quickfixes individually.
- Code actions target the file on DISK at the published line numbers — save
  the buffer before applying one, or the fix may land on stale coordinates.

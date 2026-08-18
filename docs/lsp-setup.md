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

## Notes

- macOS: the first run of a downloaded (unsigned) binary needs
  `xattr -d com.apple.quarantine /path/to/lucidlint`.
- The LSP publishes the same findings as the gate, including the
  `fix:` directives in the messages — an agent editing in the editor sees
  the exact command to run for each finding.

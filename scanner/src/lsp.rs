//! `lucidlint --lsp` — a stdio JSON-RPC language server.
//!
//! The scan core runs IN-PROCESS: buffers from didOpen/didChange are fed to
//! `scan_source` directly, so a keystroke costs a parse, not a process. The
//! per-file families plus complexity (CC >= 15) become diagnostics on every
//! change; on SAVE the whole repo is re-scanned in-process so the repo-wide
//! families (duplicate, unused — meaningless for a single buffer) join the
//! saved file's diagnostics at their gate severity (the review-log §10
//! edit-time gap: transient dead/duplicate states were never flagged).
//!
//! Editors point at: `lucidlint --lsp`

use crate::config::{load_lucidlint_config, LucidConfig};
use crate::{scan_source_lsp, FileScan, Finding};

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// LSP DiagnosticSeverity: 1 Error, 2 Warning.
fn severity_of(f: &Finding) -> i64 {
    if f.severity == "warn" {
        2
    } else {
        1
    }
}

/// Per-file findings for one buffer: all per-file kinds minus the
/// repo-wide families (duplicate/unused need the whole repo), plus a
/// complexity diagnostic for functions at/above the gate threshold. The
/// repo config's silencing rules (global + per-path ignores) are applied
/// here so the LSP agrees with the gate.
/// One finding as an LSP diagnostic: gate severity, the full fix command in
/// the message so the client can show how to fix it.
fn finding_diag(f: &Finding, source: &str) -> serde_json::Value {
    let line = f.line.saturating_sub(1); // LSP lines are 0-based
    let line_len = source.lines().nth(line).map(str::len).unwrap_or(0);
    // col (1-based, schema 3) pins the anchor node — twins on one line
    // highlight separately; 0 keeps the whole-line span
    let start_char = if f.col > 0 { (f.col - 1).min(line_len) } else { 0 };
    serde_json::json!({
        "range": {
            "start": {"line": line, "character": start_char},
            "end": {"line": line, "character": line_len},
        },
        "severity": severity_of(f),
        "source": "lucidlint",
        "message": crate::common::full_fix_command(&f.file, f.line, &f.message),
    })
}

/// The finding's `— fix: <directive>` tail, if any — the exact argument
/// sequence the client runs after `lucidlint fix` (`long-param-list
/// --fix-name <Options>` keeps its flags). Findings without a directive are
/// not auto-fixable.
fn fix_directive_from_message(message: &str) -> Option<&str> {
    message.rsplit_once("— fix: ").map(|(_, directive)| directive.trim())
}

/// The fixer kind: the directive's first token (the flags stay out of the
/// code-action title).
fn fix_kind_from_directive(directive: &str) -> &str {
    directive.split_whitespace().next().unwrap_or(directive)
}

/// The directive as runnable CLI ARGV tokens: `--fix-name` maps to the
/// parser's actual `--name` flag (the same mapping full_fix_command applies
/// to the expanded message), so a client can execute the tokens without
/// shell-joining. Placeholder names (`<Options>`) stay verbatim — the CLI
/// refuses them with an explicit message; capable clients prompt and
/// substitute.
fn directive_tokens(directive: &str) -> Vec<String> {
    let mut toks: Vec<String> = Vec::new();
    let mut prev_flag = false;
    for t in directive.split_whitespace() {
        if prev_flag {
            // the slot after --fix-name travels under --name
            toks.push(t.to_string());
            prev_flag = false;
            continue;
        }
        if t == "--fix-name" {
            toks.push("--name".to_string());
            prev_flag = true;
            continue;
        }
        toks.push(t.to_string());
    }
    toks
}

pub fn diagnostics_for(scan: &FileScan, source: &str, filter: Option<(&LucidConfig, &str)>) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for f in scan.findings.iter().filter(|f| {
        f.kind != "duplicate" && f.kind != "unused" && !filter.is_some_and(|(cfg, rel)| cfg.is_ignored(&f.kind, rel))
    }) {
        out.push(finding_diag(f, source));
    }
    for e in &scan.cc {
        if e.cc >= 15 && !filter.is_some_and(|(cfg, rel)| cfg.is_ignored("complexity", rel)) {
            let line = e.line.saturating_sub(1);
            let line_len = source.lines().nth(line).map(str::len).unwrap_or(0);
            out.push(serde_json::json!({
                "range": {
                    "start": {"line": line, "character": 0},
                    "end": {"line": line, "character": line_len},
                },
                "severity": 1,
                "source": "lucidlint",
                "message": crate::common::full_fix_command(
                    &e.file,
                    e.line,
                    &crate::common::complexity_message(e.cc, e.shape, &e.shape_detail),
                ),
            }));
        }
    }
    out
}

/// One Content-Length-framed JSON-RPC message on the wire — the one framing
/// helper every outgoing message goes through.
/// The three-part write — one statement so a single line-level suppression
/// covers the whole frame.
fn write_frame(out: &mut impl Write, header: &[u8], body: &[u8]) -> std::io::Result<()> {
    out.write_all(header)?;
    out.write_all(body)?;
    out.flush()
}

fn write_message(out: &mut impl Write, msg: serde_json::Value) {
    let body = msg.to_string();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    // the client may have closed the pipe — a failed diagnostic write ends
    // the session either way; run() exits when the next read returns None
    // lucidlint: ignore swallow best-effort diagnostic write — see above
    let _ = write_frame(out, header.as_bytes(), body.as_bytes());
}

fn publish(uri: &str, diagnostics: &[serde_json::Value], out: &mut impl Write) {
    write_message(
        out,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {"uri": uri, "diagnostics": diagnostics},
        }),
    );
}

/// file:///path/to/x.py -> the path; anything else is used as-is.
fn uri_to_path(uri: &str) -> String {
    let p = uri
        .strip_prefix("file://")
        .unwrap_or(uri)
        // percent-decode the path component (spaces as %20, etc.)
        .replace("%20", " ")
        .replace("%23", "#")
        .replace("%3F", "?");
    // file:///tmp/x keeps its leading / (an absolute path); file://tmp/x
    // is a relative URI — strip nothing
    p.to_string()
}

fn scan_buffer(uri: &str, text: &str) -> FileScan {
    let mut scan = scan_source_lsp(text, &uri_to_path(uri));
    scan.file_name = uri_to_path(uri);
    scan
}
/// The repo-wide verdict for every Python file: per-file findings plus the
/// families that need the whole repo (duplicate, unused) — the same merge the
/// gate runs. The LSP's per-buffer scan cannot produce these (a single buffer
/// cannot know who references a function), so the editor never showed the
/// gate's repo-wide findings — the review-log edit-time gap. Runs on save,
/// in-process, from disk. The merge lives in main (the composition root —
/// the scan-core modules stay standalone, layers test).
pub fn repo_wide_findings(root: &Path) -> std::collections::HashMap<String, Vec<Finding>> {
    crate::repo_wide_scan(root)
}
/// A repo-wide finding as an LSP diagnostic — gate severity, the full fix
/// command in the message, the repo config's silencing applied.
fn repo_wide_diag(f: &Finding, rel: &str, cfg: &LucidConfig) -> Option<serde_json::Value> {
    if cfg.is_ignored(&f.kind, rel) {
        return None;
    }
    let line = f.line.saturating_sub(1);
    Some(serde_json::json!({
        "range": {
            "start": {"line": line, "character": 0},
            "end": {"line": line, "character": 1000},
        },
        "severity": severity_of(f),
        "source": "lucidlint",
        "message": crate::common::full_fix_command(&f.file, f.line, &f.message),
    }))
}

/// One Content-Length-framed JSON-RPC message from stdin; None on EOF.
fn read_message<R: BufRead>(input: &mut R) -> Option<serde_json::Value> {
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {}
            Err(_) => return None,
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // header terminator
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            length = rest.trim().parse::<usize>().ok();
        }
    }
    let n = length?;
    let mut body = vec![0u8; n];
    if input.read_exact(&mut body).is_err() {
        return None;
    }
    serde_json::from_slice(&body).ok()
}

fn send_response(id: &serde_json::Value, result: serde_json::Value, out: &mut impl Write) {
    write_message(out, serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

/// The server's per-workspace state: open buffers, the workspace root, and
/// the loaded repo config (the LSP must apply the SAME silencing rules as
/// the gate — a config-ignored finding shown by the LSP while the gate hides
/// it is exactly what broke the agent's trust in the LSP).
pub struct LspState {
    pub documents: HashMap<String, String>,
    root: Option<PathBuf>,
    config: Option<LucidConfig>,
}

impl LspState {
    pub fn new() -> Self {
        LspState {
            documents: HashMap::new(),
            root: None,
            config: None,
        }
    }

    /// The nearest repo root for a file — walk up to `.lucidlint.toml` or
    /// `.git` (the editor may not send a workspace root).
    fn derive_root(file: &Path) -> Option<PathBuf> {
        let mut dir = file.parent()?;
        loop {
            if dir.join(".lucidlint.toml").is_file() || dir.join(".git").exists() {
                return Some(dir.to_path_buf());
            }
            dir = dir.parent()?;
        }
    }

    /// The (repo-relative path, config) for a uri — None until a root is
    /// known. The config is small; a per-scan load keeps it fresh when the
    /// repo's .lucidlint.toml changes while the server runs.
    fn filtering_for(&mut self, uri: &str) -> Option<(String, LucidConfig)> {
        let path = PathBuf::from(uri_to_path(uri));
        if self.root.is_none() {
            self.root = Self::derive_root(&path);
            self.config = self.root.as_ref().map(|r| load_lucidlint_config(r));
        }
        let root = self.root.as_ref()?;
        let cfg = self.config.get_or_insert_with(|| load_lucidlint_config(root));
        let rel = path.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/");
        Some((rel, cfg.clone()))
    }
}

impl LspState {
    /// The `textDocument/codeAction` request: one quickfix per finding with a
    /// fix directive in range (the directive the client runs as
    /// `lucidlint fix <directive> --file <path> --line <n>`), plus one
    /// source.fixAll — the LSP surface for the auto-fix engine.
    fn handle_code_action(&mut self, msg: &serde_json::Value, out: &mut impl Write) -> bool {
        let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let uri = msg["params"]["textDocument"]["uri"].as_str().unwrap_or("").to_string();
        let params = &msg["params"];
        let start_line = params["range"]["start"]["line"].as_u64().unwrap_or(0) as usize;
        let end_line = params["range"]["end"]["line"].as_u64().unwrap_or(u64::MAX) as usize;
        let Some(text) = self.documents.get(&uri).cloned() else {
            send_response(&id, serde_json::json!([]), out);
            return true;
        };
        let scan = scan_buffer(&uri, &text);
        let ctx = self.filtering_for(&uri);
        let mut actions: Vec<serde_json::Value> = Vec::new();
        let mut has_fix = false;
        for f in scan.findings.iter().filter(|f| {
            f.kind != "duplicate"
                && f.kind != "unused"
                && !ctx.as_ref().is_some_and(|(rel, cfg)| cfg.is_ignored(&f.kind, rel))
        }) {
            let Some(directive) = fix_directive_from_message(&f.message) else {
                continue;
            };
            let line = f.line.saturating_sub(1); // LSP lines are 0-based
            if line < start_line || line > end_line {
                continue;
            }
            has_fix = true;
            let kind = fix_kind_from_directive(directive);
            let needs_name = crate::rules_gen::NAME_REQUIRED_KINDS.contains(&kind);
            let title = if needs_name {
                format!("Apply lucidlint fix: {kind} (needs --name)")
            } else {
                format!("Apply lucidlint fix: {kind}")
            };
            actions.push(serde_json::json!({
                "title": title,
                "kind": "quickfix",
                "diagnostics": [finding_diag(f, &text)],
                "data": {"needsName": needs_name},
                "command": {
                    "title": title,
                    "command": "lucidlint.applyFix",
                    "arguments": [uri, f.line, directive_tokens(directive)],
                },
            }));
        }
        if has_fix {
            actions.push(serde_json::json!({
                "title": "Fix all lucidlint findings in file",
                "kind": "source.fixAll",
                "command": {
                    "title": "Fix all lucidlint findings in file",
                    "command": "lucidlint.fixAll",
                    "arguments": [uri],
                },
            }));
        }
        send_response(&id, serde_json::json!(actions), out);
        true
    }

    /// The didSave repo-wide verdict: the duplicate/unused findings that the
    /// per-buffer scan cannot see, appended to the buffer's diagnostics.
    fn append_repo_wide_diags(&self, rel: &str, cfg: &LucidConfig, diags: &mut Vec<serde_json::Value>) {
        if let Some(root) = self.root.as_ref() {
            let by_file = repo_wide_findings(root);
            if let Some(fs) = by_file.get(rel) {
                for f in fs {
                    if let Some(d) = repo_wide_diag(f, rel, cfg) {
                        diags.push(d);
                    }
                }
            }
        }
    }
}

pub fn dispatch(state: &mut LspState, msg: &serde_json::Value, out: &mut impl Write) -> bool {
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(serde_json::Value::Null);
    match method {
        "initialize" => {
            let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
            // capture the workspace root so config-ignores apply from the
            // first scan (rootUri, then workspaceFolders[0])
            let root_uri = params["rootUri"]
                .as_str()
                .or_else(|| {
                    params["workspaceFolders"]
                        .as_array()
                        .and_then(|a| a.first())
                        .and_then(|w| w["uri"].as_str())
                })
                .map(|u| uri_to_path(u).to_string());
            if let Some(r) = root_uri {
                let root = PathBuf::from(r);
                state.config = Some(load_lucidlint_config(&root));
                state.root = Some(root);
            }
            send_response(
                &id,
                serde_json::json!({
                    "capabilities": {
                        "textDocumentSync": {"openClose": true, "change": 1},
                        "serverInfo": {"name": "lucidlint", "version": "0.3.0"}
                    }
                }),
                out,
            );
        }
        "shutdown" => {
            let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
            send_response(&id, serde_json::Value::Null, out);
        }
        "exit" => return false,
        "textDocument/codeAction" => return state.handle_code_action(msg, out),
        "textDocument/didOpen" => {
            let doc = &params["textDocument"];
            let uri = doc["uri"].as_str().unwrap_or("").to_string();
            let text = doc["text"].as_str().unwrap_or("").to_string();
            let scan = scan_buffer(&uri, &text);
            let ctx = state.filtering_for(&uri);
            let diags = diagnostics_for(&scan, &text, ctx.as_ref().map(|(r, c)| (c, r.as_str())));
            publish(&uri, &diags, out);
            state.documents.insert(uri.clone(), text);
        }
        "textDocument/didChange" => {
            let doc = &params["textDocument"];
            let uri = doc["uri"].as_str().unwrap_or("").to_string();
            // change: 1 = full sync — the last content change is the text
            let text = params["contentChanges"]
                .as_array()
                .and_then(|c| c.last())
                .and_then(|c| c["text"].as_str())
                .unwrap_or("")
                .to_string();
            let scan = scan_buffer(&uri, &text);
            let ctx = state.filtering_for(&uri);
            let diags = diagnostics_for(&scan, &text, ctx.as_ref().map(|(r, c)| (c, r.as_str())));
            publish(&uri, &diags, out);
            state.documents.insert(uri, text);
        }
        "textDocument/didSave" => {
            let doc = &params["textDocument"];
            let uri = doc["uri"].as_str().unwrap_or("").to_string();
            if let Some(text) = state.documents.get(&uri).cloned() {
                let scan = scan_buffer(&uri, &text);
                let ctx = state.filtering_for(&uri);
                let mut diags = diagnostics_for(&scan, &text, ctx.as_ref().map(|(r, c)| (c, r.as_str())));
                // the repo-wide verdict on save: duplicate/unused need the
                // whole repo, so the per-buffer scan cannot show them —
                // publish them now that the file is on disk (review-log §10:
                // transient dead/duplicate states were never flagged at edit
                // time). One full-repo scan per save, in-process.
                if let Some((rel, cfg)) = ctx.as_ref() {
                    state.append_repo_wide_diags(rel, cfg, &mut diags);
                }
                publish(&uri, &diags, out);
            }
        }
        "textDocument/didClose" => {
            let doc = &params["textDocument"];
            let uri = doc["uri"].as_str().unwrap_or("").to_string();
            publish(&uri, &[], out); // clear the gutter on close
            state.documents.remove(&uri);
        }
        _ => {} // unknown methods and notifications are ignored
    }
    true
}

pub fn run() {
    let mut state = LspState::new();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    while let Some(msg) = read_message(&mut input) {
        if !dispatch(&mut state, &msg, &mut out) {
            return; // exit request
        }
    }
    // EOF — client gone
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_mapping() {
        let warn = Finding {
            file: "x.py".into(),
            line: 1,
            col: 0,
            function: String::new(),
            kind: "magic-number".into(),
            severity: "warn".into(),
            message: "m".into(),
        };
        let fail = Finding {
            file: "x.py".into(),
            line: 2,
            col: 0,
            function: String::new(),
            kind: "except".into(),
            severity: "fail".into(),
            message: "e".into(),
        };
        assert_eq!(severity_of(&warn), 2);
        assert_eq!(severity_of(&fail), 1);
    }

    #[test]
    fn diagnostics_exclude_repo_wide_and_map_lines() {
        // scan_source alone emits per-file findings only — duplicate/unused
        // never appear (they materialize in the CLI's repo-wide merge), so
        // the magic number is the surviving family; lines map 0-based
        let scan = scan_buffer("file:///tmp/buf.py", "def f():\n    return a * 60\n");
        let diags = diagnostics_for(&scan, "def f():\n    return a * 60\n", None);
        let kinds: Vec<&str> = diags
            .iter()
            .filter_map(|d| d["message"].as_str())
            .map(|m| {
                if m.contains("magic number") {
                    "magic"
                } else if m.contains("complexity") {
                    "cc"
                } else {
                    "other"
                }
            })
            .collect();
        assert!(kinds.contains(&"magic"), "{kinds:?}");
        assert!(!diags
            .iter()
            .any(|d| d["message"].as_str().is_some_and(|m| m.contains("never referenced"))));
        let magic = diags
            .iter()
            .find(|d| d["message"].as_str().is_some_and(|m| m.contains("magic number")))
            .unwrap();
        assert_eq!(magic["range"]["start"]["line"], 1); // 0-based line 2
        assert_eq!(magic["range"]["end"]["character"], 17); // "    return a * 60"
    }

    #[test]
    fn uri_path_mapping() {
        // file:/// keeps its absolute root; percent-encoding is decoded
        assert_eq!(uri_to_path("file:///a/b.py"), "/a/b.py");
        assert_eq!(uri_to_path("file:///tmp/my%20file.py"), "/tmp/my file.py");
        assert_eq!(uri_to_path("plain.py"), "plain.py");
    }

    #[test]
    fn dispatch_initialize_responds_with_capabilities() {
        let mut docs = LspState::new();
        let mut out = Vec::new();
        let msg = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        assert!(dispatch(&mut docs, &msg, &mut out));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("textDocumentSync"));
        assert!(text.contains("\"id\":1"));
    }

    #[test]
    fn dispatch_didopen_publishes_findings_and_caches_text() {
        let mut docs = LspState::new();
        let mut out = Vec::new();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {"textDocument": {"uri": "file:///tmp/buf.py", "text": "def f():\n    return a * 60\n"}}
        });
        assert!(dispatch(&mut docs, &msg, &mut out));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("publishDiagnostics"));
        assert!(text.contains("magic number"));
        assert!(docs.documents.contains_key("file:///tmp/buf.py"));
    }

    #[test]
    fn dispatch_codeaction_lists_fixable_findings_with_directives() {
        let mut docs = LspState::new();
        let mut out = Vec::new();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {"textDocument": {"uri": "file:///tmp/buf.py", "text": "def run(a, b, c, d, e, f):\n    return a\n"}}
        });
        assert!(dispatch(&mut docs, &msg, &mut out));
        out.clear();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/codeAction",
            "id": 1,
            "params": {
                "textDocument": {"uri": "file:///tmp/buf.py"},
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 20}}
            }
        });
        assert!(dispatch(&mut docs, &msg, &mut out));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("lucidlint.applyFix"), "the fix command: {text}");
        assert!(
            text.contains("(needs --name)"),
            "the title names the requirement: {text}"
        );
        assert!(text.contains("\"needsName\":true"), "the data marker: {text}");
        // the directive travels as runnable argv tokens, --fix-name mapped
        // to the parser's --name — no shell joining on the client
        assert!(
            text.contains("\"arguments\":[\"file:///tmp/buf.py\",1,[\"long-param-list\",\"--name\",\"<Options>\"]]"),
            "the command args: {text}"
        );
        assert!(text.contains("source.fixAll"), "the fix-all action: {text}");
    }

    #[test]
    fn dispatch_codeaction_marks_mechanical_fixes_applicable() {
        // undeclared-attribute needs no name: needsName false, bare token,
        // and the title carries no caveat
        let mut docs = LspState::new();
        let mut out = Vec::new();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {"textDocument": {"uri": "file:///tmp/buf.py", "text": "class C:\n    def __init__(self):\n        self.done = False\n"}}
        });
        assert!(dispatch(&mut docs, &msg, &mut out));
        out.clear();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/codeAction",
            "id": 4,
            "params": {
                "textDocument": {"uri": "file:///tmp/buf.py"},
                "range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 25}}
            }
        });
        assert!(dispatch(&mut docs, &msg, &mut out));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("undeclared-attribute"), "the kind: {text}");
        assert!(text.contains("\"needsName\":false"), "mechanical: {text}");
        assert!(!text.contains("needs --name"), "no caveat: {text}");
    }

    #[test]
    fn dispatch_codeaction_respects_the_requested_range() {
        // the finding sits on the def line (0); a range covering only the
        // body (line 1) must yield no actions
        let mut docs = LspState::new();
        let mut out = Vec::new();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {"textDocument": {"uri": "file:///tmp/buf.py", "text": "def run(a, b, c, d, e, f):\n    return a\n"}}
        });
        assert!(dispatch(&mut docs, &msg, &mut out));
        out.clear();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/codeAction",
            "id": 2,
            "params": {
                "textDocument": {"uri": "file:///tmp/buf.py"},
                "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 10}}
            }
        });
        assert!(dispatch(&mut docs, &msg, &mut out));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\"result\":[]"), "no actions outside the range: {text}");
        assert!(!text.contains("applyFix"), "no fix command: {text}");
    }

    #[test]
    fn dispatch_codeaction_unknown_doc_returns_empty() {
        let mut docs = LspState::new();
        let mut out = Vec::new();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/codeAction",
            "id": 3,
            "params": {
                "textDocument": {"uri": "file:///tmp/other.py"},
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}
            }
        });
        assert!(dispatch(&mut docs, &msg, &mut out));
        assert!(String::from_utf8(out).unwrap().contains("\"result\":[]"));
    }

    #[test]
    fn dispatch_didsave_republishes_cached_document() {
        let mut docs = LspState::new();
        docs.documents.insert(
            "file:///tmp/buf.py".to_string(),
            "def f():\n    return a * 60\n".to_string(),
        );
        let mut out = Vec::new();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didSave",
            "params": {"textDocument": {"uri": "file:///tmp/buf.py"}}
        });
        assert!(dispatch(&mut docs, &msg, &mut out));
        assert!(String::from_utf8(out).unwrap().contains("magic number"));
    }

    #[test]
    fn dispatch_didsave_unknown_doc_publishes_nothing() {
        let mut docs = LspState::new();
        let mut out = Vec::new();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didSave",
            "params": {"textDocument": {"uri": "file:///tmp/other.py"}}
        });
        assert!(dispatch(&mut docs, &msg, &mut out));
        assert!(out.is_empty());
    }

    #[test]
    fn dispatch_didchange_full_sync_republishes() {
        let mut docs = LspState::new();
        let mut out = Vec::new();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": {"uri": "file:///tmp/buf.py"},
                "contentChanges": [{"text": "def f():\n    return a * 60\n"}]
            }
        });
        assert!(dispatch(&mut docs, &msg, &mut out));
        assert!(String::from_utf8(out).unwrap().contains("magic number"));
        assert!(docs.documents.contains_key("file:///tmp/buf.py"));
    }

    #[test]
    fn dispatch_didclose_clears_gutter_and_doc() {
        let mut docs = LspState::new();
        docs.documents.insert("file:///tmp/buf.py".to_string(), "x".to_string());
        let mut out = Vec::new();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": {"textDocument": {"uri": "file:///tmp/buf.py"}}
        });
        assert!(dispatch(&mut docs, &msg, &mut out));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\"diagnostics\":[]"));
        assert!(!docs.documents.contains_key("file:///tmp/buf.py"));
    }

    #[test]
    fn dispatch_exit_and_shutdown() {
        let mut docs = LspState::new();
        let mut out = Vec::new();
        let msg = serde_json::json!({"jsonrpc": "2.0", "method": "exit"});
        assert!(!dispatch(&mut docs, &msg, &mut out));
        let msg = serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"});
        assert!(dispatch(&mut docs, &msg, &mut out));
        assert!(String::from_utf8(out).unwrap().contains("\"result\":null"));
    }

    #[test]
    fn dispatch_unknown_method_is_ignored() {
        let mut docs = LspState::new();
        let mut out = Vec::new();
        let msg = serde_json::json!({"jsonrpc": "2.0", "method": "$/someThing"});
        assert!(dispatch(&mut docs, &msg, &mut out));
        assert!(out.is_empty());
    }

    #[test]
    fn framing_roundtrip() {
        // a framed message parses back into the JSON value
        let payload = r#"{"jsonrpc":"2.0","method":"exit"}"#;
        let framed = format!("Content-Length: {}\r\n\r\n{}", payload.len(), payload);
        let mut cursor = std::io::Cursor::new(framed);
        let msg = read_message(&mut cursor).unwrap();
        assert_eq!(msg["method"], "exit");
    }
}

// lucidlint: ignore-file fakefs integration test writes a real repo tree to a
// temp dir — same sanctioned real-FS seam as tests/test_lsp.py
#[test]
fn didsave_publishes_repo_wide_unused_for_saved_file() {
    // review-log §10: a function that is dead REPO-WIDE never showed in
    // the editor (the per-buffer scan cannot know). On save, the
    // repo-wide merge must surface it at gate severity.
    let dir = std::env::temp_dir().join(format!("lsp_rw_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir(dir.join(".git")).unwrap(); // repo-root marker for derive_root
                                                    // dead.py defines _helper referenced nowhere; live.py is unrelated —
                                                    // the per-buffer scan of dead.py alone cannot see the absence
                                                    // lucidlint: ignore fakefs the fixture writes REAL temp files — the rule's own temp-dir carve-out
    std::fs::write(dir.join("dead.py"), "def _helper():\n    return 1\n").unwrap();
    std::fs::write(dir.join("live.py"), "def live():\n    return 2\n").unwrap();
    let uri = format!("file://{}/dead.py", dir.display());
    let mut state = LspState::new();
    state
        .documents
        .insert(uri.clone(), "def _helper():\n    return 1\n".to_string());
    let mut out = Vec::new();
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didSave",
        "params": {"textDocument": {"uri": uri}}
    });
    assert!(dispatch(&mut state, &msg, &mut out));
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("never referenced"),
        "repo-wide unused must reach the save diagnostics: {text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dispatch_honors_config_ignores() {
    // the trust fix: a config-ignored family must NOT appear as an LSP
    // diagnostic — the gate silences it, and the LSP must agree
    let dir = std::env::temp_dir().join(format!("lsp_cfg_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // lucidlint: ignore fakefs a temp .lucidlint.toml config exercises the config loader — the content string is not a path
    std::fs::write(
        dir.join(".lucidlint.toml"),
        "[lucidlint]\nignore = [\"magic-number\"]\n",
    )
    .unwrap();
    let root_uri = format!("file://{}", dir.display());
    let mut state = LspState::new();
    let mut out = Vec::new();
    let init = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"rootUri": root_uri}});
    assert!(dispatch(&mut state, &init, &mut out));
    out.clear();
    let uri = format!("file://{}/app.py", dir.display());
    let open = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {"textDocument": {"uri": uri, "text": "def f():\n    return a * 60\n"}}
    });
    assert!(dispatch(&mut state, &open, &mut out));
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("publishDiagnostics"));
    assert!(!text.contains("magic number"), "{text}"); // config-ignored
    let _ = std::fs::remove_dir_all(&dir);
}

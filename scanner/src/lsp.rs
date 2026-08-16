//! `lucidlint --lsp` — a stdio JSON-RPC language server.
//!
//! The scan core runs IN-PROCESS: buffers from didOpen/didChange are fed to
//! `scan_source` directly, so a keystroke costs a parse, not a process. No
//! Python, no binary spawn, no repo state — the per-file families plus
//! complexity (CC >= 15) become diagnostics on save; the repo-wide families
//! (duplicate, unused) are meaningless for a single buffer and are dropped.
//!
//! Editors point at: `lucidlint --lsp`

use crate::{scan_source_lsp, FileScan, Finding};
use std::collections::HashMap;
use std::io::{BufRead, Write};

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
/// complexity diagnostic for functions at/above the gate threshold.
pub fn diagnostics_for(scan: &FileScan, source: &str) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for f in scan
        .findings
        .iter()
        .filter(|f| f.kind != "duplicate" && f.kind != "unused")
    {
        let line = f.line.saturating_sub(1); // LSP lines are 0-based
        let line_len = source.lines().nth(line).map(str::len).unwrap_or(0);
        out.push(serde_json::json!({
            "range": {
                "start": {"line": line, "character": 0},
                "end": {"line": line, "character": line_len},
            },
            "severity": severity_of(f),
            "source": "lucidlint",
            "message": crate::common::full_fix_command(&f.file, f.line, &f.message),
        }));
    }
    for e in &scan.cc {
        if e.cc >= 15 {
            let line = e.line.saturating_sub(1);
            let line_len = source.lines().nth(line).map(str::len).unwrap_or(0);
            out.push(serde_json::json!({
                "range": {
                    "start": {"line": line, "character": 0},
                    "end": {"line": line, "character": line_len},
                },
                "severity": 1,
                "source": "lucidlint",
                "message": format!(
                    "cyclomatic complexity {} (>= 15) — extract part of this function into a named method — fix: extract-method (preview without --fix-name; apply with --fix-name <name>)",
                    e.cc
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

pub fn dispatch(documents: &mut HashMap<String, String>, msg: &serde_json::Value, out: &mut impl Write) -> bool {
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(serde_json::Value::Null);
    match method {
        "initialize" => {
            let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
            send_response(
                &id,
                serde_json::json!({
                    "capabilities": {
                        "textDocumentSync": {"openClose": true, "change": 1},
                        "serverInfo": {"name": "lucidlint", "version": "0.1.0"}
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
        "textDocument/didOpen" => {
            let doc = &params["textDocument"];
            let uri = doc["uri"].as_str().unwrap_or("").to_string();
            let text = doc["text"].as_str().unwrap_or("").to_string();
            let scan = scan_buffer(&uri, &text);
            let diags = diagnostics_for(&scan, &text);
            publish(&uri, &diags, out);
            documents.insert(uri.clone(), text);
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
            let diags = diagnostics_for(&scan, &text);
            publish(&uri, &diags, out);
            documents.insert(uri, text);
        }
        "textDocument/didSave" => {
            let doc = &params["textDocument"];
            let uri = doc["uri"].as_str().unwrap_or("").to_string();
            if let Some(text) = documents.get(&uri) {
                let scan = scan_buffer(&uri, text);
                let diags = diagnostics_for(&scan, text);
                publish(&uri, &diags, out);
            }
        }
        "textDocument/didClose" => {
            let doc = &params["textDocument"];
            let uri = doc["uri"].as_str().unwrap_or("").to_string();
            publish(&uri, &[], out); // clear the gutter on close
            documents.remove(&uri);
        }
        _ => {} // unknown methods and notifications are ignored
    }
    true
}

pub fn run() {
    let mut documents: HashMap<String, String> = HashMap::new();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    while let Some(msg) = read_message(&mut input) {
        if !dispatch(&mut documents, &msg, &mut out) {
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
            function: String::new(),
            kind: "magic-number".into(),
            severity: "warn".into(),
            message: "m".into(),
        };
        let fail = Finding {
            file: "x.py".into(),
            line: 2,
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
        let diags = diagnostics_for(&scan, "def f():\n    return a * 60\n");
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
        let mut docs = HashMap::new();
        let mut out = Vec::new();
        let msg = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        assert!(dispatch(&mut docs, &msg, &mut out));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("textDocumentSync"));
        assert!(text.contains("\"id\":1"));
    }

    #[test]
    fn dispatch_didopen_publishes_findings_and_caches_text() {
        let mut docs = HashMap::new();
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
        assert!(docs.contains_key("file:///tmp/buf.py"));
    }

    #[test]
    fn dispatch_didsave_republishes_cached_document() {
        let mut docs = HashMap::new();
        docs.insert(
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
        let mut docs = HashMap::new();
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
        let mut docs = HashMap::new();
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
        assert!(docs.contains_key("file:///tmp/buf.py"));
    }

    #[test]
    fn dispatch_didclose_clears_gutter_and_doc() {
        let mut docs = HashMap::new();
        docs.insert("file:///tmp/buf.py".to_string(), "x".to_string());
        let mut out = Vec::new();
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": {"textDocument": {"uri": "file:///tmp/buf.py"}}
        });
        assert!(dispatch(&mut docs, &msg, &mut out));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\"diagnostics\":[]"));
        assert!(!docs.contains_key("file:///tmp/buf.py"));
    }

    #[test]
    fn dispatch_exit_and_shutdown() {
        let mut docs = HashMap::new();
        let mut out = Vec::new();
        let msg = serde_json::json!({"jsonrpc": "2.0", "method": "exit"});
        assert!(!dispatch(&mut docs, &msg, &mut out));
        let msg = serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"});
        assert!(dispatch(&mut docs, &msg, &mut out));
        assert!(String::from_utf8(out).unwrap().contains("\"result\":null"));
    }

    #[test]
    fn dispatch_unknown_method_is_ignored() {
        let mut docs = HashMap::new();
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

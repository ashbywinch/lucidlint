//! `code-health-scan --lsp` — a stdio JSON-RPC language server.
//!
//! The scan core runs IN-PROCESS: buffers from didOpen/didChange are fed to
//! `scan_source` directly, so a keystroke costs a parse, not a process. No
//! Python, no binary spawn, no repo state — the per-file families plus
//! complexity (CC >= 15) become diagnostics on save; the repo-wide families
//! (duplicate, unused) are meaningless for a single buffer and are dropped.
//!
//! Editors point at: `code-health-scan --lsp`

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
            "source": "code-health",
            "message": f.message,
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
                "source": "code-health",
                "message": format!(
                    "cyclomatic complexity {} (>= 15) — extract each decision branch into a named method",
                    e.cc
                ),
            }));
        }
    }
    out
}

fn publish(uri: &str, diagnostics: &[serde_json::Value]) {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {"uri": uri, "diagnostics": diagnostics},
    });
    let body = msg.to_string();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(header.as_bytes());
    let _ = stdout.write_all(body.as_bytes());
    let _ = stdout.flush();
}

/// file:///path/to/x.py -> the path; anything else is used as-is.
fn uri_to_path(uri: &str) -> String {
    uri.strip_prefix("file://")
        .map(|p| p.strip_prefix('/').unwrap_or(p).to_string())
        .unwrap_or_else(|| uri.to_string())
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

fn send_response(id: &serde_json::Value, result: serde_json::Value) {
    let msg = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
    let body = msg.to_string();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(header.as_bytes());
    let _ = stdout.write_all(body.as_bytes());
    let _ = stdout.flush();
}

pub fn run() {
    let mut documents: HashMap<String, String> = HashMap::new();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    loop {
        let Some(msg) = read_message(&mut input) else {
            return; // EOF — client gone
        };
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
                            "serverInfo": {"name": "code-health", "version": "0.1.0"}
                        }
                    }),
                );
            }
            "shutdown" => {
                let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
                send_response(&id, serde_json::Value::Null);
            }
            "exit" => return,
            "textDocument/didOpen" => {
                let doc = &params["textDocument"];
                let uri = doc["uri"].as_str().unwrap_or("").to_string();
                let text = doc["text"].as_str().unwrap_or("").to_string();
                let scan = scan_buffer(&uri, &text);
                let diags = diagnostics_for(&scan, &text);
                publish(&uri, &diags);
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
                publish(&uri, &diags);
                documents.insert(uri, text);
            }
            "textDocument/didSave" => {
                let doc = &params["textDocument"];
                let uri = doc["uri"].as_str().unwrap_or("").to_string();
                if let Some(text) = documents.get(&uri) {
                    let scan = scan_buffer(&uri, text);
                    let diags = diagnostics_for(&scan, text);
                    publish(&uri, &diags);
                }
            }
            "textDocument/didClose" => {
                let doc = &params["textDocument"];
                let uri = doc["uri"].as_str().unwrap_or("").to_string();
                publish(&uri, &[]); // clear the gutter on close
                documents.remove(&uri);
            }
            _ => {} // unknown methods and notifications are ignored
        }
    }
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
        assert_eq!(uri_to_path("file:///a/b.py"), "a/b.py");
        assert_eq!(uri_to_path("plain.py"), "plain.py");
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

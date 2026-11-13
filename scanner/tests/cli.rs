//! The binary's CLI contract — the one entry point the orchestrator, the LSP
//! and a user actually run. The unit tests exercise the scan internals; these
//! drive the built binary the way a caller does: argument parsing, the JSON
//! wire shape, the version flag, and the `--fix` refusal.

use std::path::PathBuf;
use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_lucidlint")
}

/// A private temp project per test; removed on drop.
struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("lucidlint_cli_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("pkg")).expect("the temp project is writable");
        Self { root }
    }

    fn write(&self, rel: &str, source: &str) -> String {
        let path = self.root.join(rel);
        std::fs::write(&path, source).expect("the fixture file is writable");
        path.to_string_lossy().to_string()
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(binary())
            .args(args)
            .current_dir(&self.root)
            .output()
            .expect("the binary runs")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn scan_emits_the_json_contract() {
    let project = Project::new("scan");
    let file = project.write(
        "pkg/a.py",
        "def f():\n    try:\n        g()\n    except Exception:\n        pass\n",
    );
    let output = project.run(&["--root", &project.root.to_string_lossy(), &file]);
    assert!(output.status.success(), "{output:?}");

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(json["schema_version"], 4);
    assert!(json["header"].is_string());
    let findings = json["findings"].as_array().expect("findings is an array");
    assert!(
        findings
            .iter()
            .any(|f| f["kind"] == "swallow" && f["severity"] == "fail"),
        "{findings:?}"
    );
    for finding in findings {
        for key in ["file", "line", "col", "function", "kind", "severity", "message"] {
            assert!(finding.get(key).is_some(), "finding lacks {key}: {finding:?}");
        }
    }
}

#[test]
fn version_flag_reports_the_binary_version() {
    let project = Project::new("version");
    let output = project.run(&["--version"]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.starts_with("lucidlint "), "{text}");
}

#[test]
fn fix_refuses_a_kind_with_no_rust_engine() {
    let project = Project::new("fix");
    let file = project.write("pkg/lib.rs", "pub fn f() -> i32 {\n    1\n}\n");
    let spec = format!(r#"{{"kind":"bogus","file":"{file}","line":1}}"#);
    let output = project.run(&["--fix", &spec]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("has no Rust auto-fix"), "{text}");
}

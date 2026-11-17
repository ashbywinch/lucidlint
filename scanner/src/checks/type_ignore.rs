//! `type-ignore` and `noqa`: a blanket suppression comment hides whichever
//! family happens to fire next, so the comment itself is a finding.

use ruff_python_ast::token::Tokens;

use crate::finding::Finding;
use crate::suppressions::comment_lines;

/// `# type: ignore` without a why (a second comment on the line) is a finding.
pub fn type_ignore_findings(source: &str, file: &str, tokens: &Tokens) -> Vec<Finding> {
    let mut out = Vec::new();
    for (ln, text) in comment_lines(source, tokens) {
        if !text.contains("type: ignore") {
            continue;
        }
        let rest = text.split_once("type: ignore").map(|(_, r)| r).unwrap_or("");
        if !rest.contains('#') {
            out.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                            file: file.to_string(),
                            line: ln,
                            function: String::new(),
                            kind: "type-ignore".into(),
                            severity: "fail".into(),
                            message: format!(
                                "# type: ignore at line {ln} without a why — a suppression is itself a finding: add a comment explaining why the checker is wrong"
                            ), });
        }
    }
    out
}

/// `# noqa` / `# pragma: no cover` without a why (a second comment on the
/// line) is a finding — the same why-detection heuristic as
/// `type_ignore_findings`, over the same comment-token stream.
/// Does an `# noqa` / `# pragma: no cover` comment carry a reason?
/// `rest` is everything after the marker. Accepts the house format — a
/// reason on the SAME comment line (``# noqa: BLE001 — reason``, a second
/// `#` comment, or bare prose) — and still rejects the code-only form
/// (``# noqa: BLE001`` with nothing after the code).
fn noqa_reason(rest: &str) -> bool {
    let rest = rest.trim_start();
    let rest = match rest.strip_prefix(':') {
        Some(r) => {
            // `: CODE [reason]` — the code is the first token; a reason must
            // follow it. `# noqa: BLE001` (no reason) leaves nothing.
            let r = r.trim_start();
            match r.find(char::is_whitespace) {
                Some(i) => r[i..].trim(),
                None => "",
            }
        }
        None => rest, // bare `# noqa` / prose — accept a prose reason as-is
    };
    !rest.is_empty()
}

pub fn noqa_findings(source: &str, file: &str, tokens: &Tokens) -> Vec<Finding> {
    let mut out = Vec::new();
    for (ln, text) in comment_lines(source, tokens) {
        let marker = if text.contains("# noqa") {
            "# noqa"
        } else if text.contains("# pragma: no cover") {
            "# pragma: no cover"
        } else {
            continue;
        };
        let rest = text.split_once(marker).map(|(_, r)| r).unwrap_or("");
        if !noqa_reason(rest) {
            out.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                col: 0,
                file: file.to_string(),
                line: ln,
                function: String::new(),
                kind: "noqa".into(),
                severity: "fail".into(),
                message: "# noqa without a why — explain what the checker gets wrong".to_string(),
            });
        }
    }
    out
}

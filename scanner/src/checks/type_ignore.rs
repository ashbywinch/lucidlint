//! `type-ignore`: a `# type: ignore` without a why hides whatever the checker
//! would have said next, so the comment itself is a finding.

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

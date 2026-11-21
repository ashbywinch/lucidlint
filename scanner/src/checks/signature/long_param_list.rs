//! `long-param-list`: a def with more than five parameters (a leading self/cls
//! excluded) — the signature is doing too much.

use ruff_python_ast::StmtFunctionDef;
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

use super::stub_body::StubBody;

/// The long-parameter-list rule.
/// The parameter count above which a signature is a long-param-list finding.
const LONG_PARAMS_MIN: usize = 5;

pub struct LongParamList;

impl LongParamList {
    /// Report every signature with more than five real parameters. A trivial
    /// stub is exempt: the arity is the protocol's, and a parameter object
    /// would BREAK the override (review: urllib's redirect_request).
    pub fn findings(state: &mut ScanState, f: &StmtFunctionDef, source: &str) {
        let mut n = f.parameters.posonlyargs.len() + f.parameters.args.len() + f.parameters.kwonlyargs.len();
        if let Some(first) = f.parameters.posonlyargs.first().or_else(|| f.parameters.args.first()) {
            let name = first.parameter.name.as_str();
            if name == "self" || name == "cls" {
                n -= 1;
            }
        }
        if n > LONG_PARAMS_MIN && !StubBody::is_trivial(&f.body) {
            state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
            file: state.file.to_string(),
            line: Source::line_of(source, f.name.range().start()),
            function: f.name.to_string(),
            kind: "long-param-list".into(),
            severity: "fail".into(),
            message: format!(
                "{n} parameters — the call site cannot see what each value means, and no name says what they are together. Group the parameters that belong together into one object named with a domain noun. — fix: long-param-list --fix-name <Options>"
            ), });
        }
    }
}

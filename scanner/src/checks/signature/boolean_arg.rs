//! `boolean-arg`: a positional call argument that is a literal True/False —
//! the intent is unreadable at the call site; it should be a named keyword.

use ruff_python_ast::{Expr, ExprCall};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

/// Callee names whose trailing positional boolean is a lookup DEFAULT, not a
/// flag — the parameter is positional-only in CPython, so the rule's own
/// "keyword it" prescription cannot apply.
const LOOKUP_DEFAULT_CALLEES: &[&str] = &["get", "getattr", "setdefault", "pop"];

/// The boolean-argument rule.
pub struct BooleanArg;

impl BooleanArg {
    /// Report every positional literal `True`/`False` argument, except the
    /// trailing default of a lookup-with-default call.
    pub fn findings(state: &mut ScanState, call: &ExprCall, source: &str) {
        let fn_name = state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        // only ATTRIBUTE receivers (cfg.get, os.environ.get) and the bare
        // getattr builtin are lookup-with-default calls — a user function
        // literally named `get`/`setdefault`/`pop` is not, and must stay
        // flagged (review bot)
        let bare_lookup = matches!(call.func.as_ref(), Expr::Name(n) if n.id.as_str() == "getattr");
        let attr_lookup =
            matches!(call.func.as_ref(), Expr::Attribute(a) if LOOKUP_DEFAULT_CALLEES.contains(&a.attr.as_str()));
        let lookup_default = bare_lookup || attr_lookup;
        for (i, arg) in call.arguments.args.iter().enumerate() {
            if let Expr::BooleanLiteral(_) = arg {
                // the trailing positional boolean on a lookup call is the
                // DEFAULT, not a flag — positional-only, cannot be keyworded
                if lookup_default && i + 1 == call.arguments.args.len() {
                    continue;
                }
                state.findings.push(Finding {
                    logical_start: None,
                    seam_members: Vec::new(),
                    col: 0,
                    file: state.file.to_string(),
                    line: Source::line_of(source, arg.range().start()),
                    function: fn_name.clone(),
                    kind: "boolean-arg".into(),
                    severity: "fail".into(),
                    message: "boolean literal argument — name it: f(..., retry=True)".to_string(),
                });
            }
        }
    }
}

//! `detached-method`: a method whose body never references its receiver — it
//! does not touch instance state, so it does not belong in the class.

use ruff_python_ast::{Expr, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

use super::super::function_facts::FunctionFacts;
use super::stub_body::StubBody;

/// The detached-method rule.
pub struct DetachedMethod;

impl DetachedMethod {
    /// Warn on a method that never touches its receiver.
    pub fn findings(state: &mut ScanState, f: &StmtFunctionDef, source: &str) {
        let first = f.parameters.posonlyargs.first().or_else(|| f.parameters.args.first());
        let Some(receiver) = first else {
            return;
        };
        let recv = receiver.parameter.name.id.as_str();
        let decorated_class_level = f.decorator_list.iter().any(
            |d| matches!(&d.expression, Expr::Name(n) if n.id.as_str() == "classmethod" || n.id.as_str() == "staticmethod"),
        );
        if decorated_class_level {
            return;
        }
        // __init__/__new__ can never be staticmethods
        if f.name.as_str() == "__init__" || f.name.as_str() == "__new__" {
            return;
        }
        // a trivial stub (`...`, `pass`, docstring, `return None`, a lone raise)
        // is a protocol/interface placeholder — the binding is the interface's
        // contract, not a local judgment call (long-param-list's stub rationale;
        // houses: CommuteRouterLike.get_commute)
        if StubBody::is_trivial(&f.body) {
            return;
        }
        // super() needs the binding even when the body never names the receiver
        // (super().__init__(v) carries no literal self)
        if FunctionFacts::body_refs_name(&f.body, "super") {
            return;
        }
        // an override's binding is the base class's contract, not a local call
        if f.decorator_list.iter().any(|d| {
            matches!(&d.expression, Expr::Name(n) if n.id.as_str() == "override")
                || matches!(&d.expression, Expr::Attribute(a) if a.attr.as_str() == "override")
        }) {
            return;
        }
        if FunctionFacts::body_refs_name(&f.body, recv) {
            return;
        }
        let fn_name = f.name.to_string();
        let line = Source::line_of(source, f.name.range().start());
        state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                file: state.file.to_string(),
                line,
                function: fn_name.clone(),
                kind: "detached-method".into(),
                severity: "warn".into(),
                message: format!(
                    "method '{fn_name}' never uses '{recv}' — it does not touch instance state; make it a @staticmethod or move it out of the class"
                ), });
    }
}

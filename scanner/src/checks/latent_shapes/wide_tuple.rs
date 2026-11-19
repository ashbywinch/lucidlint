//! `wide-tuple`: a tuple annotation with 3+ fixed elements — a parameter,
//! return, or variable whose positions carry meaning the call site cannot see.

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

/// The wide-tuple rule: the annotation walk and the finding it emits.
pub struct WideTuple;

impl WideTuple {
    /// Report every 3+-element fixed tuple annotation in the module.
    pub fn findings(state: &mut ScanState, body: &[Stmt]) {
        Self::walk(state, body, "");
    }

    /// The annotation walk: every scope, with the enclosing function name for
    /// attribution (module-level annotations carry an empty owner).
    fn walk(state: &mut ScanState, body: &[Stmt], owner: &str) {
        for s in body {
            match s {
                Stmt::FunctionDef(f) => {
                    for a in f
                        .parameters
                        .posonlyargs
                        .iter()
                        .chain(&f.parameters.args)
                        .chain(&f.parameters.kwonlyargs)
                    {
                        Self::emit(
                            state,
                            a.annotation(),
                            &format!("parameter '{}' of '{}'", a.parameter.name, f.name),
                            Source::line_of(state.source, a.range().start()),
                            f.name.as_str(),
                        );
                    }
                    Self::emit(
                        state,
                        f.returns.as_deref(),
                        &format!("the return type of '{}'", f.name),
                        Source::line_of(state.source, f.name.range().start()),
                        f.name.as_str(),
                    );
                    Self::walk(state, &f.body, f.name.as_str());
                }
                Stmt::ClassDef(c) => Self::walk(state, &c.body, owner),
                Stmt::Try(t) => {
                    Self::walk(state, &t.body, owner);
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                        Self::walk(state, &eh.body, owner);
                    }
                    Self::walk(state, &t.orelse, owner);
                    Self::walk(state, &t.finalbody, owner);
                }
                Stmt::For(f) => {
                    Self::walk(state, &f.body, owner);
                    Self::walk(state, &f.orelse, owner);
                }
                Stmt::While(w) => {
                    Self::walk(state, &w.body, owner);
                    Self::walk(state, &w.orelse, owner);
                }
                Stmt::If(i) => {
                    Self::walk(state, &i.body, owner);
                    for clause in &i.elif_else_clauses {
                        Self::walk(state, &clause.body, owner);
                    }
                }
                Stmt::With(w) => Self::walk(state, &w.body, owner),
                Stmt::AnnAssign(a) => {
                    if let Expr::Name(n) = a.target.as_ref() {
                        Self::emit(
                            state,
                            Some(a.annotation.as_ref()),
                            &format!("'{}'", n.id.as_str()),
                            Source::line_of(state.source, a.target.range().start()),
                            owner,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    /// Fixed element count of a `tuple[...]` annotation — None when the
    /// annotation is not a tuple, is unparameterized, or is variadic
    /// (`tuple[X, ...]`: a homogeneous sequence, not a record).
    fn fixed_tuple_arity(ann: Option<&Expr>) -> Option<usize> {
        let Expr::Subscript(sub) = ann? else {
            return None;
        };
        if !matches!(sub.value.as_ref(), Expr::Name(n) if n.id.as_str() == "tuple") {
            return None;
        }
        let Expr::Tuple(t) = sub.slice.as_ref() else {
            return None;
        };
        if t.elts.iter().any(|e| matches!(e, Expr::EllipsisLiteral(_))) {
            return None;
        }
        let n = t.elts.len();
        (n >= 3).then_some(n)
    }

    /// A tuple annotation with 3+ fixed elements — a parameter, return, or
    /// variable whose positions carry meaning the call site cannot see. The
    /// smoosh (packing a long parameter list into one tuple parameter) is THIS
    /// finding with fewer lines, not a fix for it: the rule wanted a parameter
    /// object with named fields. 2-tuples are idiomatic pairs; 4+ elements
    /// fail, 3 warns (the RGB gray zone).
    fn emit(state: &mut ScanState, ann: Option<&Expr>, what: &str, line: usize, function: &str) {
        let Some(n) = Self::fixed_tuple_arity(ann) else {
            return;
        };
        let message = format!(
            "{what} is a {n}-tuple — the positions carry meaning the call site cannot see; introduce a record named with a domain noun, with named fields — fix: extract-class"
        );
        // 4+ elements fail; exactly 3 warns (the RGB gray zone). Two pushes so
        // the kind/severity literal pair stays adjacent — `make rules`' drift
        // detector reads the pair from the construction.
        if n >= 4 {
            state.findings.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                col: 0,
                file: state.file.to_string(),
                line,
                function: function.to_string(),
                kind: "wide-tuple".into(),
                severity: "fail".into(),
                message,
            });
        } else {
            state.findings.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                col: 0,
                file: state.file.to_string(),
                line,
                function: function.to_string(),
                kind: "wide-tuple".into(),
                severity: "warn".into(),
                message,
            });
        }
    }
}

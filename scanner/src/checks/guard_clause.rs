//! `guard-clause`: a nested if whose inner branch is the only work — invert and
//! return early, so the reader sees the condition once.

use ruff_python_ast::Stmt;
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

/// Guard Clauses: a chain of if-in-if ("arrow code") — the body of an if is
/// exactly another if. Depth >= 3 levels. The fix (invert to early returns)
/// is control-flow surgery; the finding names the refactoring.
/// The branch count that makes repeated guard clauses worth a named decision.
const BRANCHES_MIN: usize = 3;

pub fn guard_clause_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    fn chain_len(s: &Stmt) -> usize {
        if let Stmt::If(i) = s {
            if is_nested_if_body(i) {
                return 1 + chain_len(&i.body[0]);
            }
        }
        1
    }
    fn walk(state: &mut ScanState, stmts: &[Stmt], source: &str) {
        for s in stmts {
            if let Stmt::If(i) = s {
                let len = chain_len(s);
                if len >= BRANCHES_MIN {
                    let line = Source::line_of(source, s.range().start());
                    state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                            file: state.file.to_string(),
                                            line,
                                            function: state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default(),
                                            kind: "guard-clauses".into(),
                                            severity: "warn".into(),
                                            message: format!(
                                                "{len} levels of nested if — Replace Nested Conditional with Guard Clauses: invert the conditions to early returns"
                                            ), });
                }
                // descend: nested ifs inside the body (not the chain itself)
                if !is_nested_if_body(i) {
                    walk(state, &i.body, source);
                }
                for cl in &i.elif_else_clauses {
                    walk(state, &cl.body, source);
                }
            } else {
                walk_children(state, s, source);
            }
        }
    }
    fn walk_children(state: &mut ScanState, s: &Stmt, source: &str) {
        // descend into any nested statement containers (function/class/loop bodies)
        match s {
            Stmt::FunctionDef(f) => walk(state, &f.body, source),
            Stmt::ClassDef(cd) => walk(state, &cd.body, source),
            Stmt::For(f) => walk(state, &f.body, source),
            Stmt::While(w) => walk(state, &w.body, source),
            Stmt::Try(t) => {
                walk(state, &t.body, source);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                    walk(state, &eh.body, source);
                }
            }
            Stmt::With(w) => walk(state, &w.body, source),
            _ => {}
        }
    }
    walk(state, body, source);
}

/// The if body is a single nested if — one link of a guard chain.
fn is_nested_if_body(if_stmt: &ruff_python_ast::StmtIf) -> bool {
    if_stmt.body.len() == 1 && if_stmt.elif_else_clauses.is_empty() && matches!(if_stmt.body[0], Stmt::If(_))
}

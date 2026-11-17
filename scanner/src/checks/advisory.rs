//! Advisory refactorings: guard clause, conditional polymorphism and
//! special case — the detection half of Fowler refactorings whose fixes are
//! too involved to hand-apply.
// lucidlint: ignore-file complexity the walkers are dispatch tables over the
// statement and expression kinds — arm count is the AST's surface, not branching

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::{line_of, ScanState};

use super::latent_visitor::branch_guards;
use super::{is_empty_literal, is_nested_if_body};

/// Guard Clauses: a chain of if-in-if ("arrow code") — the body of an if is
/// exactly another if. Depth >= 3 levels. The fix (invert to early returns)
/// is control-flow surgery; the finding names the refactoring.
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
                if len >= 3 {
                    let line = line_of(source, s.range().start());
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

/// The dispatch-key (left side) of a chain-arm test — the value all arms
/// compare. None when the test is not a simple comparison on one value.
pub(super) fn dispatch_key(e: &Expr) -> Option<String> {
    match e {
        Expr::Compare(c) => match c.left.as_ref() {
            Expr::Name(n) => Some(n.id.to_string()),
            Expr::Attribute(a) => Some(format!("{}.{}", base_name_of(&a.value)?, a.attr)),
            _ => None,
        },
        Expr::Call(c) => {
            // isinstance(x, A) dispatches on x — conditional-polymorphism
            // groups chains by the VALUE they discriminate on (the
            // latent-visitor family keys by the dispatched TYPE instead,
            // see dispatch_arm)
            if let Expr::Name(n) = c.func.as_ref() {
                if n.id.as_str() == "isinstance" {
                    if let Some(Expr::Name(t)) = c.arguments.args.first() {
                        return Some(format!("isinstance({})", t.id));
                    }
                }
            }
            None
        }
        _ => None,
    }
}

pub(super) fn base_name_of(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => base_name_of(&a.value),
        _ => None,
    }
}

/// Replace Conditional with Polymorphism: an if/elif chain of >= 4 arms whose
/// tests all dispatch on the same value — the type-tag conditional.
pub fn conditional_polymorphism_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    fn walk(state: &mut ScanState, stmts: &[Stmt], source: &str) {
        for s in stmts {
            if let Stmt::If(i) = s {
                let arms: Vec<&Expr> = std::iter::once(i.test.as_ref())
                    .chain(i.elif_else_clauses.iter().filter_map(|c| c.test.as_ref()))
                    .collect();
                let chain_line = line_of(source, s.range().start());
                if arms.len() >= 4 && !state.claimed_dispatch.contains(&chain_line) {
                    let keys: Vec<Option<String>> = arms.iter().map(|t| dispatch_key(t)).collect();
                    if keys.iter().all(|k| k.is_some()) && keys.windows(2).all(|w| w[0] == w[1]) {
                        let line = chain_line;
                        state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                                    file: state.file.to_string(),
                                                    line,
                                                    function: state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default(),
                                                    kind: "conditional-polymorphism".into(),
                                                    severity: "warn".into(),
                                                    message: format!(
                                                        "{} arms dispatch on the same value ('{}') — Replace Conditional with Polymorphism: one method per case",
                                                        arms.len(),
                                                        keys[0].as_deref().unwrap_or("")
                                                    ), });
                    }
                }
                walk(state, &i.body, source);
                for cl in &i.elif_else_clauses {
                    walk(state, &cl.body, source);
                }
            } else {
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
                    _ => {}
                }
            }
        }
    }
    walk(state, body, source);
}

// ---------------------------------------------------------- scattered-instance

/// Introduce Special Case: >= 3 None/empty checks on the same name — the
/// repeated null-handling the special-case object replaces.
pub fn special_case_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let mut hits: Vec<(String, usize, usize)> = Vec::new();
    special_case_scope(body, source, &mut hits);
    hits.sort_by_key(|(_, _, line)| *line);
    for (name, n, line) in hits {
        state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                        file: state.file.to_string(),
                        line,
                        function: String::new(),
                        kind: "special-case".into(),
                        severity: "warn".into(),
                        message: format!(
                            "{n} repeated None/empty checks on '{name}' — Introduce Special Case: handle the missing or empty value once (a default or a stand-in object) instead of checking it at every use"
                        ), });
    }
}

/// One scope's repeated-check hits (name, count, first line). A function or
/// class body is its OWN scope: the same local name in two functions is two
/// unrelated values, so the counter must never add them together (a file-wide
/// count fired on common names like `found` and `callee`).
fn special_case_scope(stmts: &[Stmt], source: &str, out: &mut Vec<(String, usize, usize)>) {
    let mut counts: std::collections::HashMap<String, (usize, usize)> = std::collections::HashMap::new();
    count_scope_checks(stmts, &mut counts, source);
    for (name, (n, line)) in &counts {
        if *n >= 3 {
            out.push((name.clone(), *n, *line));
        }
    }
    for_each_nested_scope(stmts, &mut |body| special_case_scope(body, source, out));
}

/// Count this scope's None/empty checks, walking control flow but stopping at
/// a nested function/class body — those are scopes of their own.
fn count_scope_checks(stmts: &[Stmt], counts: &mut std::collections::HashMap<String, (usize, usize)>, source: &str) {
    for s in stmts {
        if let Stmt::If(i) = s {
            if let Expr::Compare(c) = i.test.as_ref() {
                if let Expr::Name(n) = c.left.as_ref() {
                    let none_check = c.ops.len() == 1
                        && matches!(c.ops[0], ruff_python_ast::CmpOp::Is)
                        && matches!(c.comparators[0], Expr::NoneLiteral(_));
                    let empty_check = c.ops.len() == 1
                        && matches!(c.ops[0], ruff_python_ast::CmpOp::Eq)
                        && is_empty_literal(&c.comparators[0]);
                    if none_check || empty_check {
                        // a fail-fast GUARD (the guarded branch raises) is not
                        // a special-case candidate: the absence IS an error, so
                        // no object can replace the repeated handling — the
                        // "give the absent object" refactoring would mask it
                        if !branch_guards(&i.body) {
                            let e = counts.entry(n.id.to_string()).or_insert((0, usize::MAX));
                            e.0 += 1;
                            e.1 = e.1.min(line_of(source, i.range().start()));
                        }
                    }
                }
            }
            count_scope_checks(&i.body, counts, source);
            for cl in &i.elif_else_clauses {
                count_scope_checks(&cl.body, counts, source);
            }
        } else {
            match s {
                Stmt::For(f) => count_scope_checks(&f.body, counts, source),
                Stmt::While(w) => count_scope_checks(&w.body, counts, source),
                Stmt::Try(t) => {
                    count_scope_checks(&t.body, counts, source);
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                        count_scope_checks(&eh.body, counts, source);
                    }
                    count_scope_checks(&t.orelse, counts, source);
                    count_scope_checks(&t.finalbody, counts, source);
                }
                // a nested function/class body is its own scope
                _ => {}
            }
        }
    }
}

/// Visit every nested function/class body inside `stmts`, through control flow
/// — the scopes `special_case_scope` recurses into.
fn for_each_nested_scope<'a>(stmts: &'a [Stmt], visit: &mut dyn FnMut(&'a [Stmt])) {
    for s in stmts {
        match s {
            Stmt::FunctionDef(f) => visit(&f.body),
            Stmt::ClassDef(c) => visit(&c.body),
            Stmt::If(i) => {
                for_each_nested_scope(&i.body, visit);
                for cl in &i.elif_else_clauses {
                    for_each_nested_scope(&cl.body, visit);
                }
            }
            Stmt::For(f) => for_each_nested_scope(&f.body, visit),
            Stmt::While(w) => for_each_nested_scope(&w.body, visit),
            Stmt::With(w) => for_each_nested_scope(&w.body, visit),
            Stmt::Try(t) => {
                for_each_nested_scope(&t.body, visit);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                    for_each_nested_scope(&eh.body, visit);
                }
                for_each_nested_scope(&t.orelse, visit);
                for_each_nested_scope(&t.finalbody, visit);
            }
            _ => {}
        }
    }
}

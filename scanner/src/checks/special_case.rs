//! `special-case`: one value checked in several nested scopes — the special case
//! belongs in one named place, not sprinkled through the flow.

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::{line_of, ScanState};

use super::latent_visitor::branch_guards;

/// `special-case`: the rule object. Its methods read one scope's repeated checks.
pub struct SpecialCase;

impl SpecialCase {
    /// Introduce Special Case: >= 3 None/empty checks on the same name — the
    /// repeated null-handling the special-case object replaces.
    pub fn findings(state: &mut ScanState, body: &[Stmt], source: &str) {
        let mut hits: Vec<(String, usize, usize)> = Vec::new();
        Self::special_case_scope(body, source, &mut hits);
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
        Self::count_scope_checks(stmts, &mut counts, source);
        for (name, (n, line)) in &counts {
            if *n >= 3 {
                out.push((name.clone(), *n, *line));
            }
        }
        Self::for_each_nested_scope(stmts, &mut |body| Self::special_case_scope(body, source, out));
    }

    /// Count this scope's None/empty checks, walking control flow but stopping at
    /// a nested function/class body — those are scopes of their own.
    fn count_scope_checks(
        stmts: &[Stmt],
        counts: &mut std::collections::HashMap<String, (usize, usize)>,
        source: &str,
    ) {
        for s in stmts {
            if let Some((name, line)) = Self::absence_check(s, source) {
                let entry = counts.entry(name).or_insert((0, usize::MAX));
                entry.0 += 1;
                entry.1 = entry.1.min(line);
            }
            match s {
                Stmt::If(i) => {
                    Self::count_scope_checks(&i.body, counts, source);
                    for cl in &i.elif_else_clauses {
                        Self::count_scope_checks(&cl.body, counts, source);
                    }
                }
                Stmt::For(f) => Self::count_scope_checks(&f.body, counts, source),
                Stmt::While(w) => Self::count_scope_checks(&w.body, counts, source),
                Stmt::Try(t) => {
                    Self::count_scope_checks(&t.body, counts, source);
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                        Self::count_scope_checks(&eh.body, counts, source);
                    }
                    Self::count_scope_checks(&t.orelse, counts, source);
                    Self::count_scope_checks(&t.finalbody, counts, source);
                }
                // a nested function/class body is its own scope
                _ => {}
            }
        }
    }

    /// The name this statement tests for None or emptiness, with the test's line —
    /// None when the statement is not such a test, or when the guarded branch is a
    /// fail-fast GUARD. A guard is not a special-case candidate: the absence IS an
    /// error, so no object can replace the repeated handling, and the "give the
    /// absent object" refactoring would mask it.
    fn absence_check(s: &Stmt, source: &str) -> Option<(String, usize)> {
        let Stmt::If(i) = s else { return None };
        let Expr::Compare(c) = i.test.as_ref() else { return None };
        let Expr::Name(n) = c.left.as_ref() else { return None };
        let none_check = c.ops.len() == 1
            && matches!(c.ops[0], ruff_python_ast::CmpOp::Is)
            && matches!(c.comparators[0], Expr::NoneLiteral(_));
        let empty_check = c.ops.len() == 1
            && matches!(c.ops[0], ruff_python_ast::CmpOp::Eq)
            && Self::is_empty_literal(&c.comparators[0]);
        if !(none_check || empty_check) || branch_guards(&i.body) {
            return None;
        }
        Some((n.id.to_string(), line_of(source, i.range().start())))
    }

    /// Visit every nested function/class body inside `stmts`, through control flow
    /// — the scopes `Self::special_case_scope` recurses into.
    fn for_each_nested_scope<'a>(stmts: &'a [Stmt], visit: &mut dyn FnMut(&'a [Stmt])) {
        for s in stmts {
            match s {
                Stmt::FunctionDef(f) => visit(&f.body),
                Stmt::ClassDef(c) => visit(&c.body),
                Stmt::If(i) => {
                    Self::for_each_nested_scope(&i.body, visit);
                    for cl in &i.elif_else_clauses {
                        Self::for_each_nested_scope(&cl.body, visit);
                    }
                }
                Stmt::For(f) => Self::for_each_nested_scope(&f.body, visit),
                Stmt::While(w) => Self::for_each_nested_scope(&w.body, visit),
                Stmt::With(w) => Self::for_each_nested_scope(&w.body, visit),
                Stmt::Try(t) => {
                    Self::for_each_nested_scope(&t.body, visit);
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                        Self::for_each_nested_scope(&eh.body, visit);
                    }
                    Self::for_each_nested_scope(&t.orelse, visit);
                    Self::for_each_nested_scope(&t.finalbody, visit);
                }
                _ => {}
            }
        }
    }

    fn is_empty_literal(e: &Expr) -> bool {
        matches!(e, Expr::StringLiteral(s) if s.value.is_empty())
            || matches!(e, Expr::List(l) if l.elts.is_empty())
            || matches!(e, Expr::Dict(d) if d.items.is_empty())
            || matches!(e, Expr::Tuple(t) if t.elts.is_empty())
    }
}

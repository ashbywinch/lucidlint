//! The decision-counting visitor backing both module-level and per-function
//! complexity. Mirrors radon's `ComplexityVisitor.generic_visit` + the
//! `visit_*` overrides.

use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
use ruff_python_ast::{Expr, Pattern, Stmt, StmtClassDef, StmtFunctionDef};

/// The decision-counting visitor backing both module-level and per-function
/// complexity. Mirrors radon's `ComplexityVisitor.generic_visit` + the
/// `visit_*` overrides.
#[derive(Default)]
pub struct CountVisitor {
    pub d: i32,
    pub no_assert: bool,
}

impl CountVisitor {
    /// Count complexity for one function body (crate-internal; the scan core
    /// calls this for its FnCc values).
    pub fn function_cc(f: &StmtFunctionDef, no_assert: bool) -> i32 {
        let mut counter = CountVisitor::default();
        counter.no_assert = no_assert;
        for stmt in &f.body {
            counter.visit_stmt(stmt);
        }
        counter.d + 1
    }

    /// The decisions a match contributes: one per case except a wildcard
    /// (`case _:` / `case x:`) — radon's `visit_Match` rule.
    fn match_cases(m: &ruff_python_ast::StmtMatch) -> i32 {
        m.cases
            .iter()
            .map(|case| {
                let wild = matches!(&case.pattern, Pattern::MatchAs(p)
                    if p.pattern.is_none() && p.name.is_none());
                if wild { 0 } else { 1 }
            })
            .sum()
    }

    /// The class's own complexity: 1 + its non-method body decisions + the sum
    /// of its methods' CCs (radon's `visit_ClassDef`: a class body starts at 1
    /// and each method contributes `functions_complexity + len(functions)`).
    pub fn class_cc(c: &StmtClassDef, no_assert: bool) -> i32 {
        let mut counter = CountVisitor::default();
        counter.no_assert = no_assert;
        for body_stmt in &c.body {
            if !matches!(body_stmt, Stmt::FunctionDef(_)) {
                counter.visit_stmt(body_stmt);
            }
        }
        let methods_cc: i32 = c
            .body
            .iter()
            .filter_map(|s| {
                if let Stmt::FunctionDef(m) = s {
                    Some(Self::function_cc(m, no_assert))
                } else {
                    None
                }
            })
            .sum();
        counter.d + 1 + methods_cc
    }
}

impl<'a> SourceOrderVisitor<'a> for CountVisitor {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => return, // own scope / 0 decisions
            Stmt::If(i) => {
                self.d += 1 + i.elif_else_clauses.iter().filter(|c| c.test.is_some()).count() as i32;
                // recurse into the test + bodies EXACTLY like radon's generic_visit
                self.visit_expr(&i.test);
                for s in &i.body {
                    self.visit_stmt(s);
                }
                for c in &i.elif_else_clauses {
                    if let Some(t) = &c.test {
                        self.visit_expr(t);
                    }
                    for s in &c.body {
                        self.visit_stmt(s);
                    }
                }
                return;
            }
            Stmt::For(f) => {
                self.d += 1 + (!f.orelse.is_empty()) as i32;
                self.visit_expr(&f.iter);
                for s in &f.body {
                    self.visit_stmt(s);
                }
                for s in &f.orelse {
                    self.visit_stmt(s);
                }
                return;
            }
            Stmt::While(w) => {
                self.d += 1 + (!w.orelse.is_empty()) as i32;
                self.visit_expr(&w.test);
                for s in &w.body {
                    self.visit_stmt(s);
                }
                for s in &w.orelse {
                    self.visit_stmt(s);
                }
                return;
            }
            // NOTE: the pinned ruff (0.0.9) has no Stmt::TryStar node —
            // try/except* (3.11 exception groups) cannot be counted here
            // until the ruff pin is bumped (see the parity review note)
            Stmt::Try(t) => {
                self.d += t.handlers.len() as i32 + (!t.orelse.is_empty()) as i32;
                for s in &t.body {
                    self.visit_stmt(s);
                }
                for handler in &t.handlers {
                    let eh = match handler {
                        ruff_python_ast::ExceptHandler::ExceptHandler(eh) => eh,
                    };
                    for s in &eh.body {
                        self.visit_stmt(s);
                    }
                }
                for s in &t.orelse {
                    self.visit_stmt(s);
                }
                for s in &t.finalbody {
                    self.visit_stmt(s);
                }
                return;
            }
            Stmt::Assert(_) => {
                if !self.no_assert {
                    self.d += 1;
                }
                // radon's visit_Assert never recurses
                return;
            }
            Stmt::Match(m) => {
                self.d += Self::match_cases(m);
                self.visit_expr(&m.subject);
                for case in &m.cases {
                    for s in &case.body {
                        self.visit_stmt(s);
                    }
                }
                return;
            }
            Stmt::Return(r) => {
                if let Some(e) = &r.value {
                    self.visit_expr(e);
                }
                return;
            }
            Stmt::Expr(e) => {
                self.visit_expr(&e.value);
                return;
            }
            Stmt::Assign(a) => {
                self.visit_expr(&a.value);
                return;
            }
            Stmt::AugAssign(a) => {
                self.visit_expr(&a.value);
                return;
            }
            Stmt::AnnAssign(a) => {
                if let Some(v) = &a.value {
                    self.visit_expr(v);
                }
                return;
            }
            _ => {}
        }
        // remaining: With, AsyncWith, Raise, etc — walk children generically
        walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::BoolOp(b) => self.d += b.values.len().saturating_sub(1) as i32,
            Expr::If(_) => self.d += 1,
            Expr::Lambda(_) => { /* +0, body walked by the default walk_expr */ }
            Expr::ListComp(c) => {
                self.d += c.generators.len() as i32;
                self.d += c.generators.iter().map(|g| g.ifs.len() as i32).sum::<i32>();
            }
            Expr::SetComp(c) => {
                self.d += c.generators.len() as i32;
                self.d += c.generators.iter().map(|g| g.ifs.len() as i32).sum::<i32>();
            }
            Expr::DictComp(c) => {
                self.d += c.generators.len() as i32;
                self.d += c.generators.iter().map(|g| g.ifs.len() as i32).sum::<i32>();
            }
            Expr::Generator(c) => {
                self.d += c.generators.len() as i32;
                self.d += c.generators.iter().map(|g| g.ifs.len() as i32).sum::<i32>();
            }
            _ => {}
        }
        walk_expr(self, expr);
    }
}

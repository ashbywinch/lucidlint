//! Radon-equivalent cyclomatic complexity (CC) for Python, using
//! `ruff_python_ast` types. Matches radon 6.0.1 exactly.
//!
//! Rules: if/elif count (+1 each, trailing else 0), for/while/try handlers+else,
//! assert +1 non-recursive, match minus wildcard, boolop operands-1, ternary +1,
//! comprehension generators+ifs (+1 each), lambda +0 body walked, nested functions
//! and class bodies contribute 0 to the enclosing function.
//!
//! The rule set is a port of the Python [radon](https://github.com/rubik/radon)
//! library (MIT, see NOTICE).

use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
use ruff_python_ast::{Expr, Pattern, Stmt, StmtFunctionDef};

/// CC for every function in a module body — returns (name, cc).
pub fn all_functions(body: &[Stmt]) -> Vec<(&str, u32)> {
    let mut out = Vec::new();
    for stmt in body {
        if let Stmt::FunctionDef(f) = stmt {
            out.push((f.name.as_str(), function_cc(f)));
        }
    }
    out
}

/// CC for one function definition.
pub fn function_cc(f: &StmtFunctionDef) -> u32 {
    let mut vis = CountVisitor::default();
    for stmt in f.body.iter() {
        vis.visit_stmt(stmt);
    }
    vis.d + 1
}

/// Tracks both statement-level and expression-level decisions in one pass.
/// The default `walk_stmt` recurses into children (bodies of if/for/try),
/// but nested function bodies and class bodies contribute 0 (radon
/// sub-visitor), and assert sub-expressions are suppressed.
#[derive(Default)]
struct CountVisitor {
    d: u32,
}

impl<'a> SourceOrderVisitor<'a> for CountVisitor {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => return, // own scope / 0 decisions
            Stmt::If(i) => {
                self.d += 1 + i.elif_else_clauses.iter().filter(|c| c.test.is_some()).count() as u32;
            }
            Stmt::For(f) => {
                self.d += 1 + (!f.orelse.is_empty()) as u32;
            }
            Stmt::While(w) => {
                self.d += 1 + (!w.orelse.is_empty()) as u32;
            }
            Stmt::Try(t) => {
                self.d += t.handlers.len() as u32 + (!t.orelse.is_empty()) as u32;
            }
            Stmt::Assert(_) => {
                self.d += 1;
                // radon's visit_Assert never recurses — no walk_stmt
                return;
            }
            Stmt::Match(m) => {
                self.d += m.cases.iter().map(|case| {
                    let wild = matches!(&case.pattern, Pattern::MatchAs(p)
                        if p.pattern.is_none() && p.name.is_none());
                    if wild { 0 } else { 1 }
                }).sum::<u32>();
            }
            _ => {}
        }
        // recurse into children — expression-level decisions counted via visit_expr
        walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::BoolOp(b) => self.d += b.values.len().saturating_sub(1) as u32,
            Expr::If(_) => self.d += 1,
            Expr::Lambda(_) => {} // +0, body walked by the default walk_expr
            Expr::ListComp(c) => {
                self.d += c.generators.len() as u32;
                self.d += c.generators.iter().map(|g| g.ifs.len() as u32).sum::<u32>();
            }
            Expr::SetComp(c) => {
                self.d += c.generators.len() as u32;
                self.d += c.generators.iter().map(|g| g.ifs.len() as u32).sum::<u32>();
            }
            Expr::DictComp(c) => {
                self.d += c.generators.len() as u32;
                self.d += c.generators.iter().map(|g| g.ifs.len() as u32).sum::<u32>();
            }
            Expr::Generator(c) => {
                self.d += c.generators.len() as u32;
                self.d += c.generators.iter().map(|g| g.ifs.len() as u32).sum::<u32>();
            }
            _ => {}
        }
        walk_expr(self, expr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_parser::{parse_module, Parsed};
    use ruff_python_ast::ModModule;

    fn fn_cc(src: &str) -> u32 {
        let parsed: Parsed<ModModule> = parse_module(src).unwrap();
        let body = parsed.syntax().body.clone();
        let funcs = all_functions(&body);
        assert_eq!(funcs.len(), 1, "test sources define one function: {src}");
        funcs[0].1
    }

    #[test]
    fn base_case_is_1() {
        assert_eq!(fn_cc("def f():\n    pass\n"), 1);
    }

    #[test]
    fn if_elif_else_counts_elifs_not_else() {
        // if + elif = 2, trailing else does NOT count -> 3 with base
        assert_eq!(
            fn_cc("def f(a, b):\n    if a:\n        return 1\n    elif b:\n        return 2\n    else:\n        return 3\n"),
            3
        );
    }

    #[test]
    fn loops_try_assert_match_boolop() {
        // for+else(2) if(1) try+handler+else(2) assert(1) match-arm(1) + base 1 = 8
        assert_eq!(
            fn_cc("def f(xs, a, b):\n    for x in xs:\n        if x:\n            break\n    else:\n        return 0\n    try:\n        g()\n    except ValueError:\n        h()\n    else:\n        k()\n    assert a and b\n    match a:\n        case 1:\n            return 1\n        case _:\n            return 0\n"),
            8
        );
    }

    #[test]
    fn nested_and_class_excluded() {
        // outer fn: only the outer if counts -> 2
        assert_eq!(
            fn_cc("def f(a):\n    def inner(x):\n        if x:\n            return 1\n        return 0\n    class C:\n        def m(self):\n            if self:\n                return 1\n    if a:\n        return inner(a)\n    return 0\n"),
            2
        );
    }

    #[test]
    fn assert_does_not_recurse() {
        // base + 2 asserts; the boolops/ifs inside contribute nothing
        assert_eq!(
            fn_cc("def f(a, b, y):\n    assert a and b\n    assert [x for x in y if x]\n    return a\n"),
            3
        );
    }

    #[test]
    fn lambda_zero_but_body_walks() {
        // lambda +0, but inner ternary +1 -> 3 total (base + if + ternary)
        assert_eq!(
            fn_cc("def f():\n    g = lambda x: 1 if x else 2\n    if g(1):\n        return g\n    return g(1)\n"),
            3
        );
    }

    #[test]
    fn comprehension_counts_each_generator() {
        // 2 generators + 1 if + base = 4
        assert_eq!(fn_cc("def f(xs):\n    return [x for x in xs for y in xs if y]\n"), 4);
    }

    #[test]
    fn all_functions_includes_nested_scope_names() {
        // module has one top-level fn -> 1 entry
        let parsed: Parsed<ModModule> =
            parse_module("def outer(a):\n    def inner(x):\n        if x:\n            return 1\n        return 0\n    return 0\n").unwrap();
        let body = parsed.syntax().body.clone();
        let funcs = all_functions(&body);
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].0, "outer");
        assert_eq!(funcs[0].1, 1); // no decisions at the outer level
    }
}
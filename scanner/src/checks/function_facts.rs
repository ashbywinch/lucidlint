//! What a function body says about itself: how many definitions it nests, the
//! names it returns and references, and its parameter names.

use std::collections::HashSet;

use ruff_python_ast::{Expr, Parameters, Stmt, StmtFunctionDef};

use super::expr_walk::{push_stmt_children, stmt_exprs};

/// What a function body says about itself: the definitions it nests, the names it returns and references, its parameters, and whether it only forwards.
pub struct FunctionFacts;

impl FunctionFacts {
    pub fn inner_function_count(fn_stmt: &Stmt) -> u32 {
        let mut count = 0;
        let mut stack: Vec<&Stmt> = Vec::new();
        if let Stmt::FunctionDef(f) = fn_stmt {
            stack.extend(f.body.iter());
        }
        while let Some(s) = stack.pop() {
            if let Stmt::FunctionDef(f) = s {
                count += 1;
                // descend into nested function bodies too — Python's ast.walk
                // counts functions at any depth
                stack.extend(f.body.iter());
            }
            push_stmt_children(s, &mut stack);
            for e in stmt_exprs(s) {
                Self::count_expr_lambdas(e, &mut count);
            }
        }
        count
    }

    pub(crate) fn count_expr_lambdas(e: &Expr, count: &mut u32) {
        // stmt_exprs already flattens the full expression tree (push_expr_tree
        // descends calls, bins, comps) — the lambda is present exactly once.
        // Any descent here would double-count.
        if matches!(e, Expr::Lambda(_)) {
            *count += 1;
        }
    }

    /// Names that surface through the function's returns: every name that
    /// appears ANYWHERE in a return expression tree (nested functions
    /// excluded) — a bare `return name`, the values of a returned dict literal,
    /// tuple elements, f-string fields, call arguments. A mutated name that
    /// surfaces in any part of the returned value surfaces the error (H4: the
    /// server.py:704 shape — the except mutates db and the returned dict
    /// carries it, so the value surfaces although `db` is not itself returned).
    pub fn returned_names(body: &[Stmt]) -> HashSet<String> {
        let mut out = HashSet::new();
        let mut stack: Vec<&Stmt> = body.iter().collect();
        while let Some(s) = stack.pop() {
            if let Stmt::FunctionDef(_) = s {
                continue; // nested function bodies excluded
            }
            if let Stmt::Return(r) = s {
                if let Some(v) = &r.value {
                    Self::collect_expr_names(v.as_ref(), &mut out);
                }
            }
            push_stmt_children(s, &mut stack);
        }
        out
    }

    /// Every `Name` in a return expression tree — the returned value's full
    /// shape (dict literal values and keys, tuple elements, f-string fields,
    /// call arguments), not just a bare name. Uses ruff's source_order walker,
    /// which descends all expression children.
    pub(crate) fn collect_expr_names(e: &Expr, out: &mut HashSet<String>) {
        use ruff_python_ast::visitor::source_order::{walk_expr, SourceOrderVisitor};
        struct NameCollector<'a>(&'a mut HashSet<String>);
        impl<'a> SourceOrderVisitor<'a> for NameCollector<'a> {
            fn visit_expr(&mut self, e: &'a Expr) {
                if let Expr::Name(n) = e {
                    self.0.insert(n.id.to_string());
                }
                walk_expr(self, e);
            }
        }
        let mut c = NameCollector(out);
        c.visit_expr(e); // visit the ROOT too — walk_expr only descends children
    }

    /// Does any expression in the body reference the given name? A source-order
    /// walk — nested functions included (a closure capturing `self` still uses
    /// instance state).
    pub(crate) fn body_refs_name(body: &[Stmt], name: &str) -> bool {
        use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
        struct Probe<'a> {
            name: &'a str,
            found: bool,
        }
        impl<'a> SourceOrderVisitor<'a> for Probe<'a> {
            fn visit_expr(&mut self, e: &'a Expr) {
                if let Expr::Name(n) = e {
                    if n.id.as_str() == self.name {
                        self.found = true;
                    }
                }
                walk_stmt_probe(self, e);
            }
        }
        // walk_expr is the trait default; we must recurse manually
        fn walk_stmt_probe<'a, V: SourceOrderVisitor<'a>>(v: &mut V, e: &'a Expr) {
            ruff_python_ast::visitor::source_order::walk_expr(v, e);
        }
        let mut probe = Probe { name, found: false };
        for s in body {
            walk_stmt(&mut probe, s);
            if probe.found {
                break;
            }
        }
        probe.found
    }

    /// The parameter names of a function (posonly, args, kwonly, vararg, kwarg).
    pub(crate) fn function_param_names(p: &Parameters, out: &mut HashSet<String>) {
        for a in &p.posonlyargs {
            out.insert(a.parameter.name.to_string());
        }
        for a in &p.args {
            out.insert(a.parameter.name.to_string());
        }
        for a in &p.kwonlyargs {
            out.insert(a.parameter.name.to_string());
        }
        if let Some(v) = &p.vararg {
            out.insert(v.name.to_string());
        }
        if let Some(k) = &p.kwarg {
            out.insert(k.name.to_string());
        }
    }

    // ------------------------------------------------------------- latent-class
    // cross-function shapes (module scope): a method in exile, an anonymous
    // tuple record, and a state-threading assembly site.

    /// The names a function's parameter list binds (positional, keyword-only,
    /// vararg, kwarg) in order.
    pub(super) fn fn_param_names(f: &StmtFunctionDef) -> Vec<String> {
        let mut out = Vec::new();
        out.extend(
            f.parameters
                .posonlyargs
                .iter()
                .chain(&f.parameters.args)
                .chain(&f.parameters.kwonlyargs)
                .map(|p| p.parameter.name.to_string()),
        );
        if let Some(v) = &f.parameters.vararg {
            out.push(v.name.to_string());
        }
        if let Some(k) = &f.parameters.kwarg {
            out.push(k.name.to_string());
        }
        out
    }

    /// Is `body` exactly `return F(<all params positionally in order>)`? The
    /// forwarder shape shared by delegating-husk and forwarding-chain.
    pub(super) fn is_pure_forwarder_body(body: &[Stmt], callee: &mut String, params: &[String]) -> bool {
        if body.len() != 1 {
            return false;
        }
        let Stmt::Return(r) = &body[0] else { return false };
        let Some(value) = r.value.as_deref() else { return false };
        let Expr::Call(c) = value else { return false };
        let Expr::Name(n) = c.func.as_ref() else { return false };
        if c.arguments.args.len() != params.len() || !c.arguments.keywords.is_empty() {
            return false;
        }
        for (arg, param) in c.arguments.args.iter().zip(params) {
            let Expr::Name(an) = arg else { return false };
            if an.id.as_str() != param.as_str() {
                return false;
            }
        }
        callee.clear();
        callee.push_str(n.id.as_str());
        true
    }
}

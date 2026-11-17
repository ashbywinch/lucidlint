// lucidlint: ignore-file complexity the AST dispatchers are single match tables over
// statement and expression kinds — arm count is the syntax's surface, not branching
// lucidlint: ignore-file boolean-arg `in_call_func` is the walk's API contract (which
// position the node sits in), not an unnamed mode — Rust has no named arguments

//! The Python AST readers the check families share: expression walks, a
//! function's returned and name-bearing expressions, annotations' base names,
//! and a class's declared fields.

use std::collections::HashSet;

use ruff_python_ast::{Expr, Parameters, Stmt, StmtClassDef};

pub(crate) fn push_stmt_children<'a>(s: &'a Stmt, stack: &mut Vec<&'a Stmt>) {
    match s {
        Stmt::If(i) => {
            for b in &i.body {
                stack.push(b);
            }
            for cl in &i.elif_else_clauses {
                for b in &cl.body {
                    stack.push(b);
                }
            }
        }
        Stmt::For(f) => {
            for b in &f.body {
                stack.push(b);
            }
            for b in &f.orelse {
                stack.push(b);
            }
        }
        Stmt::While(w) => {
            for b in &w.body {
                stack.push(b);
            }
            for b in &w.orelse {
                stack.push(b);
            }
        }
        Stmt::With(w) => {
            for b in &w.body {
                stack.push(b);
            }
        }
        Stmt::Try(t) => {
            for b in &t.body {
                stack.push(b);
            }
            for h in &t.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                for b in &eh.body {
                    stack.push(b);
                }
            }
            for b in &t.orelse {
                stack.push(b);
            }
            for b in &t.finalbody {
                stack.push(b);
            }
        }
        Stmt::Match(m) => {
            for case in &m.cases {
                for b in &case.body {
                    stack.push(b);
                }
            }
        }
        Stmt::ClassDef(c) => {
            for b in &c.body {
                stack.push(b);
            }
        }
        _ => {}
    }
}

/// The `self.<attr> = ...` assignments across a class's methods.
pub(crate) fn class_attr_names(class_body: &[Stmt]) -> HashSet<String> {
    let mut attrs = HashSet::new();
    for m in class_body {
        let Stmt::FunctionDef(f) = m else { continue };
        for s in &f.body {
            let Stmt::Assign(a) = s else { continue };
            for t in &a.targets {
                if let Expr::Attribute(at) = t {
                    if let Expr::Name(n) = at.value.as_ref() {
                        if n.id.as_str() == "self" {
                            attrs.insert(at.attr.to_string());
                        }
                    }
                }
            }
        }
    }
    attrs
}

/// Count positional reads of tuple-record dicts: `NAME[k][1]`, `a, b = NAME[k]`,
/// and `for k, (a, b) in NAME.items()` (aliases resolved through module-fn
/// call args).
///
/// Every expression reachable from *e*, INCLUDING comprehension internals
/// (elt/key/value/ifs/iter) — the reads hidden inside comprehensions.
///
/// The ONE expression traversal — every child, including comprehension
/// internals and call-func position (flagged so call-func receivers can be
/// excluded). all_exprs / the receiver-read counter / expr_writes all
/// descend through this; a new Expr variant is handled here once.
pub(crate) fn walk_expr_deep<'a>(e: &'a Expr, in_call_func: bool, f: &mut impl FnMut(&'a Expr, bool)) {
    f(e, in_call_func);
    match e {
        Expr::Call(c) => {
            walk_expr_deep(&c.func, true, f);
            for a in &c.arguments.args {
                walk_expr_deep(a, false, f);
            }
            for k in &c.arguments.keywords {
                walk_expr_deep(&k.value, false, f);
            }
        }
        Expr::Attribute(a) => walk_expr_deep(&a.value, false, f),
        Expr::Subscript(s) => {
            walk_expr_deep(&s.value, false, f);
            walk_expr_deep(&s.slice, false, f);
        }
        Expr::BinOp(b) => {
            walk_expr_deep(&b.left, false, f);
            walk_expr_deep(&b.right, false, f);
        }
        Expr::BoolOp(b) => {
            for v in &b.values {
                walk_expr_deep(v, false, f);
            }
        }
        Expr::Compare(c) => {
            walk_expr_deep(&c.left, false, f);
            for o in &c.comparators {
                walk_expr_deep(o, false, f);
            }
        }
        Expr::UnaryOp(u) => walk_expr_deep(&u.operand, false, f),
        Expr::Lambda(l) => walk_expr_deep(&l.body, false, f),
        Expr::If(i) => {
            walk_expr_deep(&i.test, false, f);
            walk_expr_deep(&i.body, false, f);
            walk_expr_deep(&i.orelse, false, f);
        }
        Expr::Named(n) => walk_expr_deep(&n.value, false, f),
        Expr::Await(a) => walk_expr_deep(&a.value, false, f),
        Expr::Starred(st) => walk_expr_deep(&st.value, false, f),
        Expr::Tuple(t) => {
            for el in &t.elts {
                walk_expr_deep(el, false, f);
            }
        }
        Expr::List(l) => {
            for el in &l.elts {
                walk_expr_deep(el, false, f);
            }
        }
        Expr::Set(st) => {
            for el in &st.elts {
                walk_expr_deep(el, false, f);
            }
        }
        Expr::ListComp(l) => {
            walk_expr_deep(&l.elt, false, f);
            for g in &l.generators {
                walk_expr_deep(&g.iter, false, f);
                for c in &g.ifs {
                    walk_expr_deep(c, false, f);
                }
            }
        }
        Expr::SetComp(sc) => {
            walk_expr_deep(&sc.elt, false, f);
            for g in &sc.generators {
                walk_expr_deep(&g.iter, false, f);
                for c in &g.ifs {
                    walk_expr_deep(c, false, f);
                }
            }
        }
        Expr::DictComp(d) => {
            if let Some(k) = &d.key {
                walk_expr_deep(k, false, f);
            }
            walk_expr_deep(&d.value, false, f);
            for g in &d.generators {
                walk_expr_deep(&g.iter, false, f);
                for c in &g.ifs {
                    walk_expr_deep(c, false, f);
                }
            }
        }
        Expr::Generator(g) => {
            walk_expr_deep(&g.elt, false, f);
            for gg in &g.generators {
                walk_expr_deep(&gg.iter, false, f);
                for c in &gg.ifs {
                    walk_expr_deep(c, false, f);
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn all_exprs<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    walk_expr_deep(e, false, &mut |x, _| out.push(x));
}

/// A class's field names (class-level annotated declarations + self.X
/// assignments in __init__), with the annotation type where present.
pub(crate) fn class_field_names(c: &StmtClassDef) -> Vec<(String, Option<String>)> {
    let mut fields: Vec<(String, Option<String>)> = Vec::new();
    for m in &c.body {
        match m {
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    let ty = annotation_base_name(a.annotation.as_ref()).map(|s| s.to_string());
                    fields.push((n.id.to_string(), ty));
                }
            }
            Stmt::FunctionDef(f) => {
                if f.name.as_str() != "__init__" {
                    continue;
                }
                for st in &f.body {
                    if let Stmt::AnnAssign(a) = st {
                        if let Expr::Attribute(at) = a.target.as_ref() {
                            if matches!(at.value.as_ref(), Expr::Name(n) if n.id.as_str() == "self") {
                                let ty = annotation_base_name(a.annotation.as_ref()).map(|s| s.to_string());
                                fields.push((at.attr.to_string(), ty));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    fields
}

pub fn stmt_exprs(s: &Stmt) -> Vec<&Expr> {
    // the expressions directly reachable from a statement (one level)
    let mut out = Vec::new();
    match s {
        Stmt::Assign(a) => {
            push_expr_tree(a.value.as_ref(), &mut out);
            for t in &a.targets {
                push_expr_tree(t, &mut out);
            }
        }
        Stmt::AnnAssign(a) => {
            if let Some(v) = &a.value {
                push_expr_tree(v.as_ref(), &mut out);
            }
        }
        Stmt::AugAssign(a) => {
            push_expr_tree(a.value.as_ref(), &mut out);
            push_expr_tree(a.target.as_ref(), &mut out);
        }
        Stmt::Expr(e) => push_expr_tree(e.value.as_ref(), &mut out),
        Stmt::Return(r) => {
            if let Some(v) = &r.value {
                push_expr_tree(v.as_ref(), &mut out);
            }
        }
        Stmt::Raise(r) => {
            if let Some(e) = &r.exc {
                push_expr_tree(e.as_ref(), &mut out);
            }
        }
        Stmt::Assert(a) => {
            push_expr_tree(a.test.as_ref(), &mut out);
        }
        Stmt::If(i) => {
            push_expr_tree(i.test.as_ref(), &mut out);
        }
        Stmt::While(w) => {
            push_expr_tree(w.test.as_ref(), &mut out);
        }
        Stmt::For(f) => {
            push_expr_tree(f.target.as_ref(), &mut out);
            push_expr_tree(f.iter.as_ref(), &mut out);
        }
        Stmt::Delete(d) => {
            for t in &d.targets {
                push_expr_tree(t, &mut out);
            }
        }
        _ => {}
    }
    out
}

pub(crate) fn push_expr_tree<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    out.push(e);
    // descend one level for Call/Attribute/Subscript/BinOp shapes
    match e {
        Expr::Call(c) => {
            push_expr_tree(c.func.as_ref(), out);
            for a in &c.arguments.args {
                push_expr_tree(a, out);
            }
            for k in &c.arguments.keywords {
                push_expr_tree(&k.value, out);
            }
        }
        Expr::Attribute(a) => push_expr_tree(&a.value, out),
        Expr::Subscript(s) => {
            push_expr_tree(s.value.as_ref(), out);
            push_expr_tree(s.slice.as_ref(), out);
        }
        Expr::BinOp(b) => {
            push_expr_tree(b.left.as_ref(), out);
            push_expr_tree(b.right.as_ref(), out);
        }
        Expr::UnaryOp(u) => push_expr_tree(&u.operand, out),
        Expr::Compare(c) => {
            push_expr_tree(c.left.as_ref(), out);
            for o in &c.comparators {
                push_expr_tree(o, out);
            }
        }
        Expr::If(e) => {
            push_expr_tree(e.test.as_ref(), out);
            push_expr_tree(e.body.as_ref(), out);
            push_expr_tree(e.orelse.as_ref(), out);
        }
        _ => {}
    }
}

/// Base name of an annotation/except-type expression: Name -> its id,
/// Attribute -> the attribute, anything else -> None (e.g. a tuple of types).
pub fn annotation_base_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => Some(a.attr.to_string()),
        _ => None,
    }
}

pub fn inner_function_count(fn_stmt: &Stmt) -> u32 {
    let mut count = 0;
    let mut stack: Vec<&Stmt> = Vec::new();
    if let Stmt::FunctionDef(f) = fn_stmt {
        for s in &f.body {
            stack.push(s);
        }
    }
    while let Some(s) = stack.pop() {
        if let Stmt::FunctionDef(f) = s {
            count += 1;
            // descend into nested function bodies too — Python's ast.walk
            // counts functions at any depth
            for b in &f.body {
                stack.push(b);
            }
        }
        push_stmt_children(s, &mut stack);
        for e in stmt_exprs(s) {
            count_expr_lambdas(e, &mut count);
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
                collect_expr_names(v.as_ref(), &mut out);
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

pub(crate) fn base_name_of(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => base_name_of(&a.value),
        _ => None,
    }
}

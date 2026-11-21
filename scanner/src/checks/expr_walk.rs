// lucidlint: ignore-file complexity the walkers are single match tables over statement
// and expression kinds — arm count is the syntax's surface, not branching
// lucidlint: ignore-file boolean-arg `in_call_func` is the walk's API contract (which
// position the node sits in), not an unnamed mode — Rust has no named arguments

// lucidlint: ignore-file boolean-arg `in_call_func` is the walk's API contract (which
// position the node sits in), not an unnamed mode — Rust has no named arguments

//! The one expression and statement walk: every child node, in one place, so a
//! new AST variant is handled once for every family that reads expressions.

use ruff_python_ast::{Expr, Stmt};

pub(crate) fn push_stmt_children<'a>(s: &'a Stmt, stack: &mut Vec<&'a Stmt>) {
    match s {
        Stmt::If(i) => {
            stack.extend(i.body.iter());
            for cl in &i.elif_else_clauses {
                stack.extend(cl.body.iter());
            }
        }
        Stmt::For(f) => {
            stack.extend(f.body.iter());
            stack.extend(f.orelse.iter());
        }
        Stmt::While(w) => {
            stack.extend(w.body.iter());
            stack.extend(w.orelse.iter());
        }
        Stmt::With(w) => {
            stack.extend(w.body.iter());
        }
        Stmt::Try(t) => {
            stack.extend(t.body.iter());
            for h in &t.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                stack.extend(eh.body.iter());
            }
            stack.extend(t.orelse.iter());
            stack.extend(t.finalbody.iter());
        }
        Stmt::Match(m) => {
            for case in &m.cases {
                stack.extend(case.body.iter());
            }
        }
        Stmt::ClassDef(c) => {
            stack.extend(c.body.iter());
        }
        _ => {}
    }
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

pub(crate) fn base_name_of(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => base_name_of(&a.value),
        _ => None,
    }
}

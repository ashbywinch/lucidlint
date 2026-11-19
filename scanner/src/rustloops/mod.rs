//! The loop family for Rust — the syn port of the Python Replace Loop with
//! Pipeline family. `RustLoops` owns the shape predicates and the fn-scope
//! binding facts; `LoopWalk` owns the per-function walk.
//!
//! Shapes, in syn terms:
//!
//! - `loop-pipeline`: the body is exactly one collection build —
//!   `acc.push(item)` (plain or behind ONE if-filter, no else) or an
//!   `acc[idx] = v` store — a combinator in disguise.
//! - `mutating-loop`: the body mutates >= 2 pieces of state that SURVIVE the
//!   loop — the changes are invisible at the call site.
//! - `loop-hoist`: exactly one surviving mutation but a body of >= 4
//!   statements — hoist the per-item step into a named helper.
//! - `loop-sequence`: >= 2 sequential loops in one function — a hidden
//!   pipeline when they share or feed state, else separate named steps.
//!
//! While loops count for the stateful verdicts but never for the pipeline
//! shape: a while condition is not an iterator. `loop {}` bodies are judged the
//! same way. The messages carry the `fix:` directives the gate wires to
//! `fix.rs` (offer equals fix).

use crate::rustscan;

mod walk;

use walk::LoopWalk;

/// The minimum flattened body statement count for a single-mutation loop to
/// carry the hoist advice — the body clearly computes more than it adds.
const HOIST_MIN_STMTS: usize = 4;

/// Mutating receiver calls — `acc.<method>(..)` WRITES `acc`. Reads (`len`,
/// `get`, `iter`, `is_empty`, ...) never count: a body that only inspects
/// the accumulator is not mutating it.
pub(crate) const MUTATING_METHODS: &[&str] = &[
    "push",
    "push_str",
    "extend",
    "insert",
    "remove",
    "pop",
    "clear",
    "sort",
    "sort_unstable",
    "truncate",
    "retain",
    "append",
    "drain",
    "put",
    "add",
    "discard",
    "update",
    "setdefault",
    "dedup",
    "resize",
    "swap_remove",
];

/// The Rust loop family — the shape predicates and the fn-scope binding facts
/// the per-function walk reads.
pub(crate) struct RustLoops;

impl RustLoops {
    /// The idents a pattern binds.
    pub(crate) fn pat_idents(pat: &syn::Pat, out: &mut Vec<String>) {
        match pat {
            syn::Pat::Ident(pi) => {
                let name = pi.ident.to_string();
                if !out.contains(&name) {
                    out.push(name);
                }
            }
            syn::Pat::Tuple(t) => {
                for e in &t.elems {
                    Self::pat_idents(e, out);
                }
            }
            syn::Pat::TupleStruct(t) => {
                for e in &t.elems {
                    Self::pat_idents(e, out);
                }
            }
            syn::Pat::Struct(s) => {
                for f in &s.fields {
                    Self::pat_idents(&f.pat, out);
                }
            }
            syn::Pat::Slice(s) => {
                for e in &s.elems {
                    Self::pat_idents(e, out);
                }
            }
            syn::Pat::Reference(r) => Self::pat_idents(&r.pat, out),
            syn::Pat::Paren(p) => Self::pat_idents(&p.pat, out),
            syn::Pat::Type(t) => Self::pat_idents(&t.pat, out),
            syn::Pat::Or(o) => {
                for c in &o.cases {
                    Self::pat_idents(c, out);
                }
            }
            _ => {}
        }
    }

    /// The base path an assigned expression writes: `x`, `x[idx]`, `x.field`
    /// all write `x`. `self.x` and bare `self` belong to the object, not the
    /// loop.
    pub(crate) fn expr_base_path(e: &syn::Expr, out: &mut Vec<String>) {
        match e {
            syn::Expr::Path(p) => {
                if p.path.segments.len() == 1 {
                    let name = p.path.segments[0].ident.to_string();
                    if name != "self" && !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
            syn::Expr::Index(i) => Self::expr_base_path(&i.expr, out),
            syn::Expr::Field(f) => Self::expr_base_path(&f.base, out),
            syn::Expr::Reference(r) => Self::expr_base_path(&r.expr, out),
            syn::Expr::Paren(p) => Self::expr_base_path(&p.expr, out),
            syn::Expr::Unary(u) => Self::expr_base_path(&u.expr, out),
            _ => {}
        }
    }
    pub(crate) fn single_push(stmt: &syn::Stmt) -> Option<String> {
        let syn::Stmt::Expr(e, Some(_)) = stmt else {
            return None;
        };
        let syn::Expr::MethodCall(m) = e else {
            return None;
        };
        if m.method != "push" || m.args.len() != 1 {
            return None;
        }
        match m.receiver.as_ref() {
            syn::Expr::Path(p) if p.path.segments.len() == 1 => Some(p.path.segments[0].ident.to_string()),
            _ => None,
        }
    }

    /// One `acc[idx] = <expr>` store statement — the indexed-build shape.
    pub(crate) fn index_store(stmt: &syn::Stmt) -> Option<String> {
        let syn::Stmt::Expr(e, Some(_)) = stmt else {
            return None;
        };
        let syn::Expr::Assign(a) = e else {
            return None;
        };
        match a.left.as_ref() {
            syn::Expr::Index(i) => match i.expr.as_ref() {
                syn::Expr::Path(p) if p.path.segments.len() == 1 => Some(p.path.segments[0].ident.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// A loop whose body is exactly one collection build — one push or one
    pub(crate) fn body_is_pipeline(stmts: &[syn::Stmt]) -> bool {
        match stmts {
            [s] if Self::single_push(s).is_some() || Self::index_store(s).is_some() => true,
            [syn::Stmt::Expr(syn::Expr::If(i), _)]
                if i.else_branch.is_none()
                    && i.then_branch.stmts.len() == 1
                    && (Self::single_push(&i.then_branch.stmts[0]).is_some()
                        || Self::index_store(&i.then_branch.stmts[0]).is_some()) =>
            {
                true
            }
            _ => false,
        }
    }
    /// The control-flow children of a statement — nested item scopes excluded.
    fn stmt_children(s: &syn::Stmt) -> Vec<&syn::Stmt> {
        match s {
            syn::Stmt::Expr(e, _) => match e {
                syn::Expr::If(i) => {
                    let mut out: Vec<&syn::Stmt> = i.then_branch.stmts.iter().collect();
                    if let Some((_, els)) = &i.else_branch {
                        if let syn::Expr::Block(b) = els.as_ref() {
                            out.extend(b.block.stmts.iter());
                        }
                    }
                    out
                }
                syn::Expr::Match(m) => {
                    let mut out = Vec::new();
                    for arm in &m.arms {
                        Self::arm_body_stmts(&arm.body, &mut out);
                    }
                    out
                }
                syn::Expr::Block(b) => b.block.stmts.iter().collect(),
                syn::Expr::ForLoop(f) => f.body.stmts.iter().collect(),
                syn::Expr::While(w) => w.body.stmts.iter().collect(),
                syn::Expr::Loop(l) => l.body.stmts.iter().collect(),
                syn::Expr::Unsafe(u) => u.block.stmts.iter().collect(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// The statement lists a match arm's body contributes — a block body walks
    /// directly; a nested if/match expression contributes its own children.
    fn arm_body_stmts<'s>(body: &'s syn::Expr, out: &mut Vec<&'s syn::Stmt>) {
        match body {
            syn::Expr::Block(b) => out.extend(b.block.stmts.iter()),
            syn::Expr::If(i) => {
                out.extend(i.then_branch.stmts.iter());
                if let Some((_, els)) = &i.else_branch {
                    if let syn::Expr::Block(b) = els.as_ref() {
                        out.extend(b.block.stmts.iter());
                    }
                }
            }
            syn::Expr::Match(m) => {
                for arm in &m.arms {
                    Self::arm_body_stmts(&arm.body, out);
                }
            }
            _ => {}
        }
    }

    /// The flattened statement count of a loop body — every control-flow child
    /// counts, nested items are not the body's business.
    fn body_stmt_count(stmts: &[syn::Stmt]) -> usize {
        fn walk(stmts: &[syn::Stmt], n: &mut usize) {
            for s in stmts {
                if matches!(s, syn::Stmt::Item(_)) {
                    continue;
                }
                *n += 1;
                for child in RustLoops::stmt_children(s) {
                    walk(std::slice::from_ref(child), n);
                }
            }
        }
        let mut n = 0;
        walk(stmts, &mut n);
        n
    }

    /// The names one statement WRITES — rebinds, compound-assigns, indexed and
    /// field stores (via their base path), and mutating receiver calls. `let`
    /// bindings are new names, not writes; nested items are new scopes.
    fn stmt_writes(stmt: &syn::Stmt, out: &mut Vec<String>) {
        let syn::Stmt::Expr(e, _) = stmt else {
            return;
        };
        match e {
            syn::Expr::Assign(a) => Self::expr_base_path(&a.left, out),
            syn::Expr::Binary(b) if rustscan::is_compound_op(&b.op) => {
                Self::expr_base_path(&b.left, out);
            }
            syn::Expr::MethodCall(m) => {
                if MUTATING_METHODS.contains(&m.method.to_string().as_str()) {
                    if let syn::Expr::Path(p) = m.receiver.as_ref() {
                        if p.path.segments.len() == 1 {
                            let name = p.path.segments[0].ident.to_string();
                            if name != "self" && !out.contains(&name) {
                                out.push(name);
                            }
                        }
                    }
                }
            }
            syn::Expr::If(i) => {
                for s in &i.then_branch.stmts {
                    Self::stmt_writes(s, out);
                }
                if let Some((_, els)) = &i.else_branch {
                    if let syn::Expr::Block(b) = els.as_ref() {
                        for s in &b.block.stmts {
                            Self::stmt_writes(s, out);
                        }
                    }
                }
            }
            syn::Expr::Match(m) => {
                for arm in &m.arms {
                    let mut kids = Vec::new();
                    Self::arm_body_stmts(&arm.body, &mut kids);
                    for s in kids {
                        Self::stmt_writes(s, out);
                    }
                }
            }
            syn::Expr::Block(b) => {
                for s in &b.block.stmts {
                    Self::stmt_writes(s, out);
                }
            }
            syn::Expr::Unsafe(u) => {
                for s in &u.block.stmts {
                    Self::stmt_writes(s, out);
                }
            }
            syn::Expr::ForLoop(f) => {
                for s in &f.body.stmts {
                    Self::stmt_writes(s, out);
                }
            }
            syn::Expr::While(w) => {
                for s in &w.body.stmts {
                    Self::stmt_writes(s, out);
                }
            }
            syn::Expr::Loop(l) => {
                for s in &l.body.stmts {
                    Self::stmt_writes(s, out);
                }
            }
            _ => {}
        }
    }

    /// Fn-scope locals bound OUTSIDE every loop body — the state a loop can
    /// survive: `let` bindings at the top level plus non-loop control-flow
    /// branches. Loop bodies are NOT descended and loop patterns are NOT
    /// bound: an iteration variable or a body-local temp is not state that
    /// outlives a pass.
    fn bind_fn_stmts(stmts: &[syn::Stmt], out: &mut std::collections::HashSet<String>) {
        for s in stmts {
            match s {
                syn::Stmt::Local(l) => {
                    let mut v = Vec::new();
                    Self::pat_idents(&l.pat, &mut v);
                    out.extend(v);
                }
                syn::Stmt::Expr(e, _) => match e {
                    syn::Expr::ForLoop(_) | syn::Expr::While(_) | syn::Expr::Loop(_) => {}
                    syn::Expr::If(i) => {
                        Self::bind_fn_stmts(&i.then_branch.stmts, out);
                        if let Some((_, els)) = &i.else_branch {
                            Self::bind_fn_expr(els, out);
                        }
                    }
                    syn::Expr::Match(m) => {
                        for arm in &m.arms {
                            Self::bind_fn_expr(&arm.body, out);
                        }
                    }
                    syn::Expr::Block(b) => Self::bind_fn_stmts(&b.block.stmts, out),
                    syn::Expr::Unsafe(u) => Self::bind_fn_stmts(&u.block.stmts, out),
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn bind_fn_expr(e: &syn::Expr, out: &mut std::collections::HashSet<String>) {
        match e {
            syn::Expr::If(i) => {
                Self::bind_fn_stmts(&i.then_branch.stmts, out);
                if let Some((_, els)) = &i.else_branch {
                    Self::bind_fn_expr(els, out);
                }
            }
            syn::Expr::Match(m) => {
                for arm in &m.arms {
                    Self::bind_fn_expr(&arm.body, out);
                }
            }
            syn::Expr::Block(b) => Self::bind_fn_stmts(&b.block.stmts, out),
            syn::Expr::Unsafe(u) => Self::bind_fn_stmts(&u.block.stmts, out),
            _ => {}
        }
    }

    /// The per-function pass: pipeline/mutating/hoist per loop at any depth,
    /// loop-sequence for the function's OUTERMOST loops.
    pub(crate) fn findings(
        state: &mut rustscan::RsState,
        sig: &syn::Signature,
        block: &syn::Block,
        fn_name: &str,
        fn_line: usize,
    ) {
        LoopWalk::findings(state, sig, block, fn_name, fn_line);
    }
}

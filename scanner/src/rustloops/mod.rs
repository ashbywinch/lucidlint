//! The loop family for Rust — the syn port of the Python Replace Loop with
//! Pipeline family. `RustLoops` owns the shape predicates and the fn-scope
//! binding facts; `LoopWalk` owns the per-function walk, the shape it derives
//! from those facts (`LoopShape`, `PipelineBuild`), and the worklist-drain
//! detection.
//!
//! Shapes, in syn terms:
//!
//! - `loop-pipeline`: the body is exactly one collection build —
//!   `acc.push(item)` (plain or behind ONE if-filter, no else) or an
//!   `acc[idx] = v` store — a combinator in disguise: `.map(..).collect()`
//!   builds the value when the loop creates the collection (over the
//!   indices for an indexed store), `.filter(..)` for the guarded write,
//!   `.extend(..)` when a push joins items onto a collection that already
//!   held some.
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
//!
//! A loop whose CONDITION removes from a name its body writes drains its own
//! worklist (`while let Some(x) = stack.pop()`): the writes are the walk's
//! state, not outputs, and none of the stateful recipes describes a
//! traversal — that loop gets no verdict. The Python pass reached the same
//! conclusion under P5 (a worklist drain "fits none of the recipes").

use crate::rustscan;

mod branches;
mod facts;
mod walk;

use branches::{BranchKind, Branches};
use facts::PipelineBuild;
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
            // a chained receiver writes its base: `m.entry(k).or_default()
            // .push(v)` writes `m`, exactly as `m.push(v)` does. A temporary
            // receiver (`build().push(v)`) has no base path and stays uncounted.
            syn::Expr::MethodCall(m) => Self::expr_base_path(&m.receiver, out),
            _ => {}
        }
    }

    /// A `&mut <name>` argument: the callee may write the name, so the loop
    /// cannot claim the state is untouched. A `&mut` borrow of a temporary
    /// (`&mut vec![..]`) has no name and says nothing about the loop's state.
    fn mut_ref_name(e: &syn::Expr) -> Option<String> {
        let syn::Expr::Reference(r) = e else {
            return None;
        };
        r.mutability?;
        let syn::Expr::Path(p) = r.expr.as_ref() else {
            return None;
        };
        (p.path.segments.len() == 1).then(|| p.path.segments[0].ident.to_string())
    }

    fn record_mut_ref(e: &syn::Expr, out: &mut Vec<String>) {
        if let Some(name) = Self::mut_ref_name(e) {
            if name != "self" && !out.contains(&name) {
                out.push(name);
            }
        }
    }

    /// The name a UFCS mutating call writes — `Vec::pop(&mut stack)`, the free
    /// spelling of `stack.pop()`. The last path segment must be a mutating
    /// method and the first argument `&mut <name>`; a shared borrow removes
    /// nothing.
    fn ufcs_mutated(c: &syn::ExprCall) -> Option<String> {
        let syn::Expr::Path(p) = c.func.as_ref() else {
            return None;
        };
        let last = p.path.segments.last()?;
        if !MUTATING_METHODS.contains(&last.ident.to_string().as_str()) {
            return None;
        }
        let syn::Expr::Reference(r) = c.args.first()? else {
            return None;
        };
        r.mutability?;
        let syn::Expr::Path(rp) = r.expr.as_ref() else {
            return None;
        };
        (rp.path.segments.len() == 1).then(|| rp.path.segments[0].ident.to_string())
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

    /// The collection a pipeline-shaped body builds and how — the `acc` of a
    /// bare `acc.push(x)` or `acc[i] = v`, plain or behind ONE if-filter with
    /// no else. `None` for every other body.
    pub(crate) fn pipeline_build(stmts: &[syn::Stmt]) -> Option<PipelineBuild> {
        let (stmt, guard) = match stmts {
            [syn::Stmt::Expr(syn::Expr::If(i), _)] if i.else_branch.is_none() && i.then_branch.stmts.len() == 1 => {
                (&i.then_branch.stmts[0], Some(i.cond.as_ref()))
            }
            [s] => (s, None),
            _ => return None,
        };
        if let Some(acc) = Self::single_push(stmt) {
            if Self::pipeline_reads(stmt, guard, &acc) {
                return None;
            }
            return Some(PipelineBuild::Push(acc));
        }
        if let Some(acc) = Self::index_store(stmt) {
            if Self::pipeline_reads(stmt, guard, &acc) {
                return None;
            }
            return Some(PipelineBuild::Indexed(acc));
        }
        None
    }

    /// Does the pipeline body read the accumulator outside its own write? The
    /// guard, the pushed item, and the store's index and value are the only
    /// other expressions; any read of the target makes the loop a scan over a
    /// growing collection, not a per-item map.
    fn pipeline_reads(stmt: &syn::Stmt, guard: Option<&syn::Expr>, acc: &str) -> bool {
        if Self::expr_mentions(guard, acc) {
            return true;
        }
        if let Some(item) = Self::push_item(stmt) {
            return Self::expr_mentions(Some(item), acc);
        }
        if let Some((index, value)) = Self::store_parts(stmt) {
            return Self::expr_mentions(Some(index), acc) || Self::expr_mentions(Some(value), acc);
        }
        false
    }

    /// Does the expression (when present) read `name`? The pipeline's item,
    /// guard and store value are checked against the accumulator; the
    /// mutation's own receiver is not.
    fn expr_mentions(e: Option<&syn::Expr>, name: &str) -> bool {
        e.is_some_and(|e| rustscan::RustShape::expr_idents(e).iter().any(|n| n == name))
    }

    fn push_item(stmt: &syn::Stmt) -> Option<&syn::Expr> {
        let syn::Stmt::Expr(syn::Expr::MethodCall(m), Some(_)) = stmt else {
            return None;
        };
        (m.method == "push" && m.args.len() == 1).then(|| &m.args[0])
    }

    /// The `(index, value)` of one `acc[idx] = value` store.
    fn store_parts(stmt: &syn::Stmt) -> Option<(&syn::Expr, &syn::Expr)> {
        let syn::Stmt::Expr(syn::Expr::Assign(a), Some(_)) = stmt else {
            return None;
        };
        let syn::Expr::Index(i) = a.left.as_ref() else {
            return None;
        };
        matches!(i.expr.as_ref(), syn::Expr::Path(p) if p.path.segments.len() == 1)
            .then(|| (i.index.as_ref(), a.right.as_ref()))
    }

    /// A loop whose body is exactly one collection build — one push or one
    /// indexed store.
    pub(crate) fn body_is_pipeline(stmts: &[syn::Stmt]) -> bool {
        Self::pipeline_build(stmts).is_some()
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
                for branch in Branches::of(s) {
                    walk(branch.stmts, n);
                }
            }
        }
        let mut n = 0;
        walk(stmts, &mut n);
        n
    }

    /// The names one statement WRITES — rebinds, compound-assigns, indexed and
    /// field stores (via their base path), mutating receiver calls (including a
    /// chained receiver), and the UFCS spelling of a mutating call. `let`
    /// bindings are new names, not writes; nested items are new scopes.
    fn stmt_writes(stmt: &syn::Stmt, out: &mut Vec<String>) {
        if let syn::Stmt::Expr(e, _) = stmt {
            match e {
                syn::Expr::Assign(a) => Self::expr_base_path(&a.left, out),
                syn::Expr::Binary(b) if rustscan::is_compound_op(&b.op) => {
                    Self::expr_base_path(&b.left, out);
                }
                syn::Expr::MethodCall(m) => {
                    if MUTATING_METHODS.contains(&m.method.to_string().as_str()) {
                        Self::expr_base_path(&m.receiver, out);
                    }
                    for arg in &m.args {
                        Self::record_mut_ref(arg, out);
                    }
                }
                syn::Expr::Call(c) => {
                    if let Some(name) = Self::ufcs_mutated(c) {
                        if name != "self" && !out.contains(&name) {
                            out.push(name);
                        }
                    }
                    for arg in &c.args {
                        Self::record_mut_ref(arg, out);
                    }
                }
                _ => {}
            }
        }
        for branch in Branches::of(stmt) {
            for s in branch.stmts {
                Self::stmt_writes(s, out);
            }
        }
    }

    /// Fn-scope locals bound OUTSIDE every loop body — the state a loop can
    /// survive: `let` bindings at the top level plus non-loop control-flow
    /// branches. Loop bodies are NOT descended and loop patterns are NOT
    /// bound: an iteration variable or a body-local temp is not state that
    /// outlives a pass.
    fn bind_fn_stmts(stmts: &[syn::Stmt], out: &mut std::collections::HashSet<String>) {
        for s in stmts {
            if let syn::Stmt::Local(l) = s {
                let mut v = Vec::new();
                Self::pat_idents(&l.pat, &mut v);
                out.extend(v);
            }
            for branch in Branches::of(s) {
                if branch.kind != BranchKind::LoopBody {
                    Self::bind_fn_stmts(branch.stmts, out);
                }
            }
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

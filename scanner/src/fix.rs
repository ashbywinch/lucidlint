//! The Rust fix surface — extract-method for `.rs` targets.
//!
//! The Python orchestrator's fix engine is libcst (Python-only). For a Rust
//! finding the extraction runs here, as a lossless TEXT edit: syn gives
//! spans, we slice the original source and splice the helper + call in, so
//! comments and formatting survive (rustfmt remains the house formatter).
//!
//! Scope — refuse anything more, honestly: a seam whose free variables are
//! a subset of the function's PARAMETERS (their types come from the
//! signature; deriving types for locals would need a type checker) and that
//! has no out-variables and no control-flow exit.

use proc_macro2::LineColumn;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{FnArg, Ident, Item, ItemFn, Pat, Stmt};

/// A file byte offset from a proc-macro2 LineColumn (1-based line, byte col).
fn byte_offset(source: &str, lc: LineColumn) -> usize {
    let (line, col) = (lc.line, lc.column);
    let mut cur = 1usize;
    for (i, b) in source.bytes().enumerate() {
        if cur == line {
            return i + col;
        }
        if b == b'\n' {
            cur += 1;
        }
    }
    source.len()
}

/// Collect `let`/loop/closure/match bindings in a statement set.
struct BindCollector<'a> {
    bound: &'a mut std::collections::HashSet<String>,
}

impl Visit<'_> for BindCollector<'_> {
    fn visit_local(&mut self, node: &syn::Local) {
        pat_bindings(&node.pat, self.bound);
    }
    fn visit_expr_for_loop(&mut self, node: &syn::ExprForLoop) {
        pat_bindings(&node.pat, self.bound);
        syn::visit::visit_expr_for_loop(self, node);
    }
    fn visit_expr_match(&mut self, node: &syn::ExprMatch) {
        for arm in &node.arms {
            pat_bindings(&arm.pat, self.bound);
        }
        syn::visit::visit_expr_match(self, node);
    }
    fn visit_item_fn(&mut self, node: &ItemFn) {
        // nested fn is a new scope — its params bind here, its body doesn't
        // contribute to ours; skip the body
        for p in &node.sig.inputs {
            if let FnArg::Typed(t) = p {
                pat_bindings(&t.pat, self.bound);
            }
        }
    }
}

fn pat_bindings(pat: &Pat, out: &mut std::collections::HashSet<String>) {
    match pat {
        Pat::Ident(pi) => {
            out.insert(pi.ident.to_string());
        }
        Pat::Tuple(t) => t.elems.iter().for_each(|e| pat_bindings(e, out)),
        Pat::Type(t) => pat_bindings(&t.pat, out),
        Pat::Reference(r) => pat_bindings(&r.pat, out),
        Pat::Struct(s) => s.fields.iter().for_each(|f| pat_bindings(&f.pat, out)),
        Pat::Or(o) => o.cases.iter().for_each(|c| pat_bindings(c, out)),
        Pat::Slice(s) => s.elems.iter().for_each(|e| pat_bindings(e, out)),
        Pat::Paren(p) => pat_bindings(&p.pat, out),
        Pat::TupleStruct(t) => t.elems.iter().for_each(|e| pat_bindings(e, out)),
        _ => {}
    }
}

struct SeamCollector<'a> {
    bound: &'a std::collections::HashSet<String>,
    params: &'a std::collections::HashSet<String>,
    free: &'a mut Vec<Ident>,
    seen: &'a mut std::collections::HashSet<String>,
    ok: &'a mut bool,
}

impl Visit<'_> for SeamCollector<'_> {
    fn visit_expr_path(&mut self, node: &syn::ExprPath) {
        let segs = &node.path.segments;
        let name = segs[0].ident.to_string();
        if name == "self" || name == "Self" {
            *self.ok = false; // the helper would need `self` — refuse
            return;
        }
        if segs.len() == 1 {
            if self.bound.contains(&name) {
                return; // a seam-local, moves with the seam
            }
            if self.params.contains(&name) {
                if self.seen.insert(name) {
                    self.free.push(segs[0].ident.clone());
                }
                return;
            }
            *self.ok = false; // an unknown name — cannot derive its type
        }
        // multi-segment: module path — the base could be a param or ambient
    }
    fn visit_expr_method_call(&mut self, node: &syn::ExprMethodCall) {
        // the method name is ambient; visit the receiver
        self.visit_expr(&node.receiver);
    }
    fn visit_expr_field(&mut self, node: &syn::ExprField) {
        // `x.field` — the member is ambient; the base path handles x
        self.visit_expr(&node.base);
    }
}

/// Names computed in the seam and read in the after-statements — out-vars.
struct OutVarCollector<'a> {
    seam_writes: &'a std::collections::HashSet<String>,
    out: &'a mut bool,
}

impl Visit<'_> for OutVarCollector<'_> {
    fn visit_expr_path(&mut self, node: &syn::ExprPath) {
        if node.path.segments.len() == 1 {
            let n = node.path.segments[0].ident.to_string();
            if self.seam_writes.contains(&n) {
                *self.out = true;
            }
        }
    }
    fn visit_expr_method_call(&mut self, node: &syn::ExprMethodCall) {
        self.visit_expr(&node.receiver);
    }
}

struct FlowExitVisitor {
    found: bool,
}

impl Visit<'_> for FlowExitVisitor {
    fn visit_expr_return(&mut self, _node: &syn::ExprReturn) {
        self.found = true;
    }
    fn visit_expr_break(&mut self, _node: &syn::ExprBreak) {
        self.found = true;
    }
    fn visit_expr_continue(&mut self, _node: &syn::ExprContinue) {
        self.found = true;
    }
}

/// Does `stmt` contain a control-flow exit ANYWHERE in its subtree? A nested
/// `return` inside an `if` still changes which function returns.
fn is_control_flow(stmt: &Stmt) -> bool {
    let mut v = FlowExitVisitor { found: false };
    v.visit_stmt(stmt);
    v.found
}

/// The names `let`-bound directly in the seam (its locals).
fn seam_bindings(stmts: &[&Stmt]) -> std::collections::HashSet<String> {
    let mut bound = std::collections::HashSet::new();
    for s in stmts {
        BindCollector { bound: &mut bound }.visit_stmt(s);
    }
    bound
}

/// The seam's free variables (fn params it reads) and whether it is safe.
fn seam_analysis(stmts: &[&Stmt], after: &[&Stmt], params: &[Ident]) -> Option<Vec<Ident>> {
    let pset: std::collections::HashSet<String> = params.iter().map(|i| i.to_string()).collect();
    let bound = seam_bindings(stmts);
    let mut free: Vec<Ident> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut ok = true;
    for s in stmts {
        SeamCollector {
            bound: &bound,
            params: &pset,
            free: &mut free,
            seen: &mut seen,
            ok: &mut ok,
        }
        .visit_stmt(s);
    }
    if !ok {
        return None;
    }
    // out-vars: any seam-local read in the after-statements
    let mut out = false;
    for s in after {
        OutVarCollector {
            seam_writes: &bound,
            out: &mut out,
        }
        .visit_stmt(s);
    }
    if out {
        return None;
    }
    Some(free)
}

/// Extract a param-only, no-out-var seam of `line`'s fn into `name`.
/// Returns Ok(rewritten source) or Err(why no seam exists) — the "nothing to
/// change" path must explain itself (review-log R1).
pub fn fix_extract_method(source: &str, line: usize, name: &str) -> Result<String, String> {
    let file = syn::parse_file(source).map_err(|_| "the file does not parse".to_string())?;
    let target = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Fn(f) if f.sig.ident.span().start().line == line => Some(f),
            _ => None,
        })
        .ok_or_else(|| format!("no function starts at line {line}"))?;
    let params: Vec<Ident> = target
        .sig
        .inputs
        .iter()
        .filter_map(|a| match a {
            FnArg::Typed(t) => match t.pat.as_ref() {
                Pat::Ident(pi) => Some(pi.ident.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let stmts: Vec<&Stmt> = target.block.stmts.iter().collect();
    if stmts.len() < 2 {
        return Err("the function's body has fewer than two statements".to_string());
    }
    // longest seam first (refuse nested — a multi-statement contiguous block)
    for len in (1..stmts.len()).rev() {
        for start in 0..=stmts.len() - len {
            let seam = &stmts[start..start + len];
            if start == 0 && start + len == stmts.len() {
                continue; // the whole body is not a seam
            }
            if seam.iter().any(|s| is_control_flow(s)) {
                continue;
            }
            let after: Vec<&Stmt> = stmts[start + len..].to_vec();
            if let Some(free) = seam_analysis(seam, &after, &params) {
                return apply(source, target, seam, &free, name).ok_or_else(|| {
                    "the seam's free variables are not a subset of the function's parameters".to_string()
                });
            }
        }
    }
    Err("no self-contained seam exists: every candidate has control flow, reads a value written in the seam (out-variable), or calls a method/`self`".to_string())
}

fn apply(source: &str, target: &ItemFn, seam: &[&Stmt], free: &[Ident], name: &str) -> Option<String> {
    let first = seam.first()?.span().start();
    let last = seam.last()?.span().end();
    let seam_start = byte_offset(source, first);
    let seam_end = byte_offset(source, last);
    // the target fn's start — insert the helper just before it
    let fn_start = byte_offset(source, target.span().start());
    // each free var's type, sliced losslessly from the signature
    let mut sig = Vec::new();
    for p in free {
        let ty_span = target.sig.inputs.iter().find_map(|a| match a {
            FnArg::Typed(t) => match t.pat.as_ref() {
                Pat::Ident(pi) if pi.ident == *p => Some(t.ty.span()),
                _ => None,
            },
            _ => None,
        })?;
        let ts = byte_offset(source, ty_span.start());
        let te = byte_offset(source, ty_span.end());
        sig.push((p.to_string(), source[ts..te].trim().to_string()));
    }
    let params: Vec<String> = sig.iter().map(|(n, t)| format!("{n}: {t}")).collect();
    let args: Vec<String> = free.iter().map(|i| i.to_string()).collect();
    // the helper, with the seam body spliced in losslessly (indent preserved
    // as written; rustfmt normalizes)
    let helper = format!(
        "fn {name}({})\n{{\n{}\n}}\n\n",
        params.join(", "),
        String::from(&source[seam_start..seam_end])
    );
    let call = format!("{}({});", name, args.join(", "));

    let mut out = String::new();
    out.push_str(&source[..fn_start]);
    out.push_str(&helper);
    out.push_str(&source[fn_start..seam_start]);
    out.push_str(&call);
    out.push_str(&source[seam_end..]);
    Some(out)
}

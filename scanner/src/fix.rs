//! The Rust fix surface — extract-method, dispatch-registry, rule-table,
//! and the loop family (loop-pipeline, loop-sequence, loop-hoist) for
//! `.rs` targets.
//!
//! The Python orchestrator's fix engine is libcst (Python-only). For a Rust
//! finding the rewrite runs here, as a lossless TEXT edit: syn gives
//! spans, we slice the original source and splice the replacement in, so
//! comments and formatting survive (rustfmt remains the house formatter).
//!
//! Scope — refuse anything more, honestly: extract-method needs a seam
//! whose free variables are a subset of the function's PARAMETERS (their
//! types come from the signature; deriving types for locals would need a
//! type checker) with no out-variables and no control-flow exit; the loop
//! fixes need the detector's exact single-accumulator shape (each refusal
//! names why, never a silent no-op).

use proc_macro2::LineColumn;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Expr, FnArg, Ident, Item, ItemFn, Lit, Pat, Stmt};

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

/// The fn whose span contains `line` — the loop fixes resolve by finding
/// line, not by position in the file (a later fn's loop must not rewrite
/// through the first fn's accumulator).
fn enclosing_fn(file: &syn::File, line: usize) -> Option<&ItemFn> {
    file.items.iter().find_map(|item| match item {
        Item::Fn(f) if f.span().start().line <= line && line <= f.span().end().line => Some(f),
        _ => None,
    })
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
    // a free PARAM read in the after-statements would be MOVED into the
    // helper by value — the after-read becomes use-of-moved-value; refuse
    let free_set: std::collections::HashSet<String> = free.iter().map(|i| i.to_string()).collect();
    if !free_set.is_empty() {
        let mut after_read = false;
        for s in after {
            AfterReadCollector {
                names: &free_set,
                out: &mut after_read,
            }
            .visit_stmt(s);
        }
        if after_read {
            return None;
        }
    }
    // a param SHADOWED by a seam-local binding that the seam also READS:
    // the read may be of the param (before the shadow) — the binding set is
    // order-insensitive, so the helper would get no argument and reference
    // an undefined name; refuse (conservative, offer-equals-fix)
    let seam_reads = seam_read_names(stmts);
    for name in &bound {
        if pset.contains(name) && seam_reads.contains(name) {
            return None;
        }
    }
    Some(free)
}

/// All bare identifiers read in a statement run.
fn seam_read_names(stmts: &[&Stmt]) -> std::collections::HashSet<String> {
    struct ReadNames(std::collections::HashSet<String>);
    impl<'ast> syn::visit::Visit<'ast> for ReadNames {
        fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
            if node.path.segments.len() == 1 {
                self.0.insert(node.path.segments[0].ident.to_string());
            }
        }
    }
    let mut v = ReadNames(std::collections::HashSet::new());
    for s in stmts {
        v.visit_stmt(s);
    }
    v.0
}

/// Does any statement READ one of `names`?
struct AfterReadCollector<'a> {
    names: &'a std::collections::HashSet<String>,
    out: &'a mut bool,
}
impl<'ast> syn::visit::Visit<'ast> for AfterReadCollector<'_> {
    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        if node.path.segments.len() == 1 {
            let n = node.path.segments[0].ident.to_string();
            if self.names.contains(&n) {
                *self.out = true;
            }
        }
    }
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
    // the fn's generic parameters (`<T: Ord>` / lifetimes) must be carried
    // into the helper — a free param of type T would otherwise reference an
    // undefined T at the helper's definition (review-log M14)
    let generics = if target.sig.generics.params.is_empty() {
        String::new()
    } else {
        let gs = byte_offset(source, target.sig.generics.span().start());
        let ge = byte_offset(source, target.sig.generics.span().end());
        source[gs..ge].to_string()
    };
    // the helper, with the seam body spliced in losslessly (indent preserved
    // as written; rustfmt normalizes)
    let helper = format!(
        "fn {name}{generics}({})\n{{\n{}\n}}\n\n",
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

/// A Rust dispatch chain (`if sel == "a" { ... } if sel == "b" { ... }`) is
/// most lucidly a `match` — Rust's idiomatic dispatch table. Lossless text
/// splice: the if-conditions become match arms, the trailing fallback the
/// `_` arm (review: run_tool's Rust analog). Returns Err(why) when the shape
/// is not a clean chain.
/// Parse contiguous dispatch arms from a statement list, starting at `first_if`.
/// Returns (selector, arms_text, lits, next_idx) or Err(why).
fn parse_dispatch_arms(
    stmts: &[syn::Stmt],
    first_if: usize,
    source: &str,
) -> Result<(String, Vec<String>, Vec<String>, usize), String> {
    let mut selector: Option<String> = None;
    let mut arms: Vec<String> = Vec::new();
    let mut lits: Vec<String> = Vec::new();
    let mut idx = first_if;
    while idx < stmts.len() {
        let syn::Stmt::Expr(e, _) = &stmts[idx] else { break };
        let syn::Expr::If(i) = e else { break };
        if i.else_branch.is_some() {
            return Err("an arm has an else branch -- the match rewrite would delete it".to_string());
        }
        if !block_ends_with_return(&i.then_branch) {
            return Err("an arm does not end in `return` -- the match arms would need matching types".to_string());
        }
        let syn::Expr::Binary(b) = i.cond.as_ref() else {
            return Err("an arm's condition is not `sel == \"lit\"`".to_string());
        };
        if !matches!(b.op, syn::BinOp::Eq(_)) {
            return Err("an arm's condition is not `sel == \"lit\"`".to_string());
        }
        let syn::Expr::Path(p) = b.left.as_ref() else {
            return Err("an arm's condition is not `sel == \"lit\"`".to_string());
        };
        if p.path.segments.len() != 1 {
            return Err("an arm's selector is not a plain name".to_string());
        }
        let sel = p.path.segments[0].ident.to_string();
        match &selector {
            None => selector = Some(sel),
            Some(s) if *s == sel => {}
            _ => return Err("the arms do not share one selector".to_string()),
        }
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s), ..
        }) = b.right.as_ref()
        else {
            return Err("an arm's comparison is not against a string literal".to_string());
        };
        let lit = s.token().to_string();
        if lits.contains(&lit) {
            return Err("two arms dispatch on the same literal".to_string());
        }
        lits.push(lit.clone());
        let block = span_text(source, i.then_branch.span())?;
        arms.push(format!("{lit} => {block}"));
        idx += 1;
    }
    let sel = selector.ok_or_else(|| "no selector found".to_string())?;
    Ok((sel, arms, lits, idx))
}

pub fn fix_dispatch_registry(source: &str, line: usize) -> Result<String, String> {
    let file = syn::parse_file(source).map_err(|_| "the file does not parse".to_string())?;
    let target = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Fn(f) if f.sig.ident.span().start().line == line => Some(f),
            _ => None,
        })
        .ok_or_else(|| format!("no function starts at line {line}"))?;
    let stmts = &target.block.stmts;
    let first_if = stmts
        .iter()
        .position(|s| matches!(s, Stmt::Expr(e, _) if matches!(e, Expr::If(_))))
        .ok_or_else(|| "the function has no if statements to convert".to_string())?;
    let (sel, arms, _lits, idx) = parse_dispatch_arms(stmts, first_if, source)?;
    if arms.len() < 3 {
        return Err("fewer than 3 dispatch arms — a registry is not worth it".to_string());
    }
    if !param_is_str(&target.sig, &sel) {
        return Err("the selector must be declared `&str` for the match rewrite".to_string());
    }
    // the fallback (statements after the chain) becomes the `_` arm
    let tail_text = if idx < stmts.len() {
        let start = byte_offset(source, stmts[idx].span().start());
        let end = byte_offset(source, stmts[stmts.len() - 1].span().end());
        source[start..end].to_string()
    } else {
        String::new()
    };
    let wild = if tail_text.trim().is_empty() {
        "_ => {}"
    } else {
        &format!("_ => {tail_text}")
    };
    let mut match_text = format!("match {sel} {{\n");
    for a in &arms {
        match_text.push_str("    ");
        match_text.push_str(a);
        match_text.push('\n');
    }
    match_text.push_str("    ");
    match_text.push_str(wild);
    match_text.push_str("\n}");
    // splice: replace [first-if start .. end of last body statement]
    let splice_start = byte_offset(source, stmts[first_if].span().start());
    let splice_end = byte_offset(source, stmts[stmts.len() - 1].span().end());
    let mut out = String::new();
    out.push_str(&source[..splice_start]);
    out.push_str(&match_text);
    out.push_str(&source[splice_end..]);
    Ok(out)
}

/// The exact source text of a span (lossless).
fn span_text(source: &str, span: proc_macro2::Span) -> Result<String, String> {
    let start = byte_offset(source, span.start());
    let end = byte_offset(source, span.end());
    Ok(source[start..end].to_string())
}

/// Does the block end with a `return ...;` statement? Only such arms are
/// match-rewritable blind: diverging arms need no matching types.
fn block_ends_with_return(block: &syn::Block) -> bool {
    matches!(block.stmts.last(), Some(syn::Stmt::Expr(syn::Expr::Return(_), Some(_))))
}

/// Is `name` a parameter declared `&str` (no lifetime)? The match rewrite
/// only compiles for a `&str` selector.
fn param_is_str(sig: &syn::Signature, name: &str) -> bool {
    sig.inputs.iter().any(|a| match a {
        syn::FnArg::Typed(pt) => {
            matches!(&*pt.pat, syn::Pat::Ident(pi) if pi.ident == name)
                && matches!(
                    pt.ty.as_ref(),
                    syn::Type::Reference(r)
                        if r.lifetime.is_none()
                            && matches!(
                                r.elem.as_ref(),
                                syn::Type::Path(tp)
                                    if tp.qself.is_none() && tp.path.is_ident("str")
                            )
                )
        }
        _ => false,
    })
}

/// One rule battery's analyzed shape: the typed params, the accumulator name,
/// and the (condition, message) arms — the latent data structure to hoist.
struct RuleBattery {
    params: Vec<(String, String)>,
    acc: String,
    arms: Vec<(String, String)>,
}

/// The shape analysis of a rule battery: `let mut <acc> = vec![];`, >= 3
/// `if <cond> { <acc>.push("msg"); }` checks (conditions read only the fn's
/// params — fn-pointer conditions cannot capture), and a trailing
/// `<acc>` expression. Err(why) when the body is not that shape.
fn rule_battery_shape(source: &str, target: &ItemFn) -> Result<RuleBattery, String> {
    let params: Vec<(String, String)> = target
        .sig
        .inputs
        .iter()
        .filter_map(|a| match a {
            FnArg::Typed(t) => match t.pat.as_ref() {
                Pat::Ident(pi) => {
                    let ts = byte_offset(source, t.ty.span().start());
                    let te = byte_offset(source, t.ty.span().end());
                    Some((pi.ident.to_string(), source[ts..te].trim().to_string()))
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    let pset: std::collections::HashSet<String> = params.iter().map(|(n, _)| n.clone()).collect();
    let stmts = &target.block.stmts;
    if stmts.len() < 2 {
        // an empty body would panic on stmts[0] — refuse cleanly
        return Err("the function body is empty".to_string());
    }
    let Stmt::Local(init) = &stmts[0] else {
        return Err("the function does not open with an accumulator binding".to_string());
    };
    let Pat::Ident(acc_pat) = &init.pat else {
        return Err("the accumulator binding is not a plain name".to_string());
    };
    let acc = acc_pat.ident.to_string();
    let mut arms: Vec<(String, String)> = Vec::new();
    let mut idx = 1usize;
    while idx < stmts.len() - 1 {
        let Stmt::Expr(e, _) = &stmts[idx] else { break };
        let Expr::If(i) = e else { break };
        let block = &i.then_branch;
        let push_stmt = block
            .stmts
            .iter()
            .find(|s| matches!(s, Stmt::Expr(se, Some(_)) if is_push_call(se, &acc)))
            .ok_or_else(|| "an arm does not push to the accumulator".to_string())?;
        let Stmt::Expr(se, Some(_)) = push_stmt else {
            unreachable!()
        };
        let Expr::MethodCall(mc) = se else { unreachable!() };
        let Expr::Lit(syn::ExprLit { lit: Lit::Str(s), .. }) = &mc.args[0] else {
            return Err("an arm pushes a non-string literal".to_string());
        };
        let cond_text = span_text(source, i.cond.span())?;
        for name in idents_in_expr(&i.cond) {
            if !pset.contains(&name) {
                return Err(format!(
                    "check reads '{name}' — not a parameter (fn-pointer conditions cannot capture)"
                ));
            }
        }
        arms.push((cond_text, s.token().to_string()));
        idx += 1;
    }
    if arms.len() < 3 {
        return Err("fewer than 3 checks — not a battery".to_string());
    }
    if idx != stmts.len() - 1 {
        // the rewrite replaces the WHOLE body (the table + collector) — any
        // statement between the checks and the trailing accumulator would be
        // silently deleted; refuse instead
        return Err("statements between the checks and the accumulator — not a pure battery".to_string());
    }
    let tail = &stmts[stmts.len() - 1];
    let Stmt::Expr(te, _) = tail else {
        return Err("the function does not end with the accumulator expression".to_string());
    };
    let Expr::Path(tp) = te else {
        return Err("the function does not end with the accumulator expression".to_string());
    };
    if tp.path.segments.len() != 1 || tp.path.segments[0].ident != acc {
        return Err("the function does not end with the accumulator expression".to_string());
    }
    Ok(RuleBattery { params, acc, arms })
}

pub fn fix_rule_table(source: &str, line: usize) -> Result<String, String> {
    let file = syn::parse_file(source).map_err(|_| "the file does not parse".to_string())?;
    let target = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Fn(f) if f.sig.ident.span().start().line == line => Some(f),
            _ => None,
        })
        .ok_or_else(|| format!("no function starts at line {line}"))?;
    let battery = rule_battery_shape(source, target)?;
    let params = &battery.params;
    let arms = &battery.arms;
    let acc = &battery.acc;
    let param_sig: Vec<String> = params.iter().map(|(n, t)| format!("{n}: {t}")).collect();
    let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
    let args = param_names.join(", ");
    // the condition predicates BEFORE the fn — `fn _rule_N(<params>) -> bool`
    // (fn-pointer conditions: Rust closures cannot form a homogeneous table,
    // so the conditions stay pure and the table pairs them with the messages)
    let mut helpers = String::new();
    for (i, (cond, _)) in arms.iter().enumerate() {
        helpers.push_str(&format!(
            "fn _rule_{}({}) -> bool {{\n    {cond}\n}}\n\n",
            i + 1,
            param_sig.join(", ")
        ));
    }
    // the hoisted (condition, violation) table + the collector — parity with
    // the Python lambda-table (the table IS the latent data structure).
    // The header slice runs from the ITEM start (attrs + visibility + fn
    // keyword) — slicing from the signature start dropped #[cfg]/#[allow]
    // attrs and `pub` (review-log H3).
    let fn_start = byte_offset(source, target.span().start());
    let body_start = byte_offset(source, target.block.brace_token.span.open().start()) + 1;
    let body_end = byte_offset(source, target.block.brace_token.span.close().end()) - 1;
    let mut new_body = String::from("\n");
    let type_only: Vec<String> = params.iter().map(|(_, t)| t.clone()).collect();
    let fn_type = format!("fn({}) -> bool", type_only.join(", "));
    new_body.push_str(&format!(
        "    let rules: [({fn_type}, &'static str); {}] = [\n",
        arms.len()
    ));
    for (i, (_, lit)) in arms.iter().enumerate() {
        new_body.push_str(&format!("        (_rule_{i}, {lit}),\n", i = i + 1));
    }
    new_body.push_str(
        "    ];
",
    );
    new_body.push_str(&format!("    let mut {acc} = vec![];\n"));
    new_body.push_str(&format!(
        "    for (_cond, _msg) in rules {{\n        if _cond({args}) {{\n            {acc}.push(_msg);\n        }}\n    }}\n"
    ));
    new_body.push_str(acc);
    // the helpers go AFTER any file-level inner attributes (`#![...]`,
    // `//!` docs) — prepending before them would break the file
    let inner_end = file.attrs.iter().map(|a| a.span().end()).max();
    let insert_at = match inner_end {
        Some(e) => byte_offset(source, e),
        None => 0,
    };
    let mut out = String::new();
    out.push_str(&source[..insert_at]);
    out.push_str(&helpers);
    out.push_str(&source[insert_at..fn_start]);
    out.push_str(&source[fn_start..body_start]);
    out.push_str(&new_body);
    out.push_str(&source[body_end..]);
    Ok(out)
}

fn is_push_call(e: &Expr, acc: &str) -> bool {
    match e {
        Expr::MethodCall(m) => {
            m.method == "push"
                && matches!(m.receiver.as_ref(), Expr::Path(p)
                    if p.path.segments.len() == 1 && p.path.segments[0].ident == acc)
        }
        _ => false,
    }
}

/// The bare identifiers referenced in an expression.
fn idents_in_expr(e: &Expr) -> Vec<String> {
    struct Idents(Vec<String>);
    impl syn::visit::Visit<'_> for Idents {
        fn visit_expr_path(&mut self, node: &syn::ExprPath) {
            if node.path.segments.len() == 1 {
                let n = node.path.segments[0].ident.to_string();
                if !self.0.contains(&n) {
                    self.0.push(n);
                }
            }
        }
    }
    let mut v = Idents(Vec::new());
    v.visit_expr(e);
    v.0
}

/// One loop's pipeline plan: the accumulator, the shape (pure-push keeps
/// the borrowed item, an if-filter narrows it, an index store needs the
/// position), and the source slices the combinator rewrite needs.
struct PipelinePlan {
    acc: String,
    filter: Option<String>,
    indexed: bool,
    push_arg: String,
    pat: String,
    iter: String,
}

/// The loop's item binding: a plain single name (never `_` or
/// destructuring — there is no name to rebind in the closure parameter).
/// Returns the binding plus whether the push sits behind an if-filter.
// lucidlint: ignore complexity the two refuses are the contract — one shape, two names for failure
fn pipeline_loop_target(f: &syn::ExprForLoop) -> Result<(String, bool), String> {
    let Pat::Ident(pi) = &*f.pat else {
        return Err("the loop pattern is not a plain binding".to_string());
    };
    let pat = pi.ident.to_string();
    if pat == "_" {
        return Err("the loop binds `_` — there is no item name to map over".to_string());
    }
    let mut bound = Vec::new();
    crate::rustloops::pat_idents(&f.pat, &mut bound);
    if bound.len() != 1 {
        return Err("the loop pattern binds more than one name".to_string());
    }
    let filtered = matches!(f.body.stmts.as_slice(), [Stmt::Expr(Expr::If(_), _)]);
    Ok((pat, filtered))
}

/// The `loop-pipeline` plan for the for-loop at `loop_idx`: the body is one
/// collection build (`rustloops::body_is_pipeline`) over a `let mut <acc>`
/// initialised to empty DIRECTLY before the loop. The accumulator must be
/// dead after the loop except through the fn's tail value — the rewrite
/// moves its binding, so any other read would break. Err(why) when the
/// shape is not losslessly rewritable.
/// Which init the plan resolves: a single loop needs its init directly
/// above; a chain member points at the fn-open shared init.
#[derive(Clone, Copy)]
enum InitScope {
    Single,
    Chain,
}

fn pipeline_plan(
    source: &str,
    target: &ItemFn,
    stmts: &[Stmt],
    loop_idx: usize,
    scope: InitScope,
) -> Result<(usize, PipelinePlan), String> {
    let Stmt::Expr(Expr::ForLoop(f), _) = &stmts[loop_idx] else {
        return Err("the statement at the finding line is not a for-loop".to_string());
    };
    let (pat, core_is_if) = pipeline_loop_target(f)?;
    // one collection build — the detector's own predicate is the contract
    if !crate::rustloops::body_is_pipeline(&f.body.stmts) {
        return Err("the loop body is not a single collection build".to_string());
    }
    let _ = core_is_if;
    // the accumulator: the push receiver / store base
    let core: &Stmt = match f.body.stmts.as_slice() {
        [s] => s,
        _ => return Err("the loop body is not a single collection build".to_string()),
    };
    let core: &Stmt = match core {
        Stmt::Expr(Expr::If(i), _) => &i.then_branch.stmts[0],
        _ => core,
    };
    let acc = crate::rustloops::single_push(core)
        .or_else(|| crate::rustloops::index_store(core))
        .ok_or_else(|| "the loop body is not a single collection build".to_string())?;
    let filter = match f.body.stmts.as_slice() {
        [Stmt::Expr(Expr::If(i), _)] => Some(span_text(source, i.cond.span())?),
        _ => None,
    };
    let push_arg = match core {
        Stmt::Expr(Expr::MethodCall(m), Some(_)) => span_text(source, m.args[0].span())?,
        Stmt::Expr(Expr::Assign(a), Some(_)) => span_text(source, a.right.span())?,
        _ => return Err("the loop body is not a single collection build".to_string()),
    };
    let indexed = crate::rustloops::index_store(core).is_some();
    let init_idx = pipeline_init_index(stmts, loop_idx, scope, &acc)?;
    // the empty check: `Vec::new()` / `vec![]` / `HashSet::new()` — a
    // non-empty init would be dropped by the rewrite; refuse instead
    pipeline_empty_init(stmts, init_idx, &acc)?;
    // the tail: the fn must END with the accumulator (possibly `return
    // <acc>` / `<acc>` with trailing `;`?) — anything after that reads it
    // is a second use the move would break. A bare `acc` tail or
    // `return acc;` tail both count; other tails refuse.
    pipeline_tail_ok(stmts, &acc)?;
    // no other read of the accumulator between init and tail: the rewrite
    // deletes the `let mut`, so a mid-body read would dangle. Sibling
    // for-loops over the same accumulator are NOT reads — the sequence
    // fixer plans each loop separately and concatenates them.
    pipeline_no_mid_read(stmts, init_idx, loop_idx, &acc)?;
    let iter = span_text(source, f.expr.span())?;
    let _ = target;
    Ok((
        init_idx,
        PipelinePlan {
            acc,
            filter,
            indexed,
            push_arg,
            pat,
            iter,
        },
    ))
}

/// The init index for a pipeline plan: DIRECTLY before the loop (skipping
/// attributes), or the fn's first statement for a shared chain init.
// lucidlint: ignore complexity the init walk is one linear scan — match-arm count, not branching
fn pipeline_init_index(stmts: &[Stmt], loop_idx: usize, scope: InitScope, acc: &str) -> Result<usize, String> {
    if matches!(scope, InitScope::Chain) {
        // the chain init is the fn's FIRST non-item statement (verified by
        // the sequence fixer) — point every member plan at it so the empty
        // check and the tail verdict agree on one accumulator
        return stmts
            .iter()
            .position(|s| !matches!(s, Stmt::Item(_)))
            .ok_or_else(|| format!("the accumulator `{acc}` has no initialiser"));
    }
    let mut init_idx = loop_idx;
    loop {
        if init_idx == 0 {
            return Err(format!(
                "the accumulator `{acc}` must be initialised to an empty collection directly before the loop"
            ));
        }
        init_idx -= 1;
        match &stmts[init_idx] {
            Stmt::Item(_) => {}
            Stmt::Local(l) => {
                let bound = match &l.pat {
                    Pat::Ident(lp) => lp.ident.to_string(),
                    Pat::Type(pt) => match pt.pat.as_ref() {
                        Pat::Ident(lp) => lp.ident.to_string(),
                        _ => {
                            return Err(format!(
                                "the statement before the loop is not `let mut {acc}` — the rewrite cannot preserve prior contents"
                            ));
                        }
                    },
                    _ => {
                        return Err(format!(
                            "the statement before the loop is not `let mut {acc}` — the rewrite cannot preserve prior contents"
                        ));
                    }
                };
                if bound != acc {
                    return Err(format!(
                        "the statement before the loop binds `{bound}` — the rewrite needs `let mut {acc}` directly above"
                    ));
                }
                if l.init.is_none() {
                    return Err(format!("the statement before the loop is not `let mut {acc} = ...`"));
                }
                return Ok(init_idx);
            }
            _ => {
                return Err(format!(
                    "the accumulator `{acc}` must be initialised to an empty collection directly before the loop — the rewrite cannot preserve prior contents"
                ));
            }
        }
    }
}

/// The init holds an empty collection — a non-empty init would be dropped
/// by the rewrite; refuse instead.
fn pipeline_empty_init(stmts: &[Stmt], init_idx: usize, acc: &str) -> Result<(), String> {
    let Stmt::Local(init) = &stmts[init_idx] else {
        return Err("the shared accumulator init is not a plain let binding".to_string());
    };
    let init_expr = init.init.as_ref().map(|b| b.expr.as_ref());
    let empty = match init_expr {
        Some(Expr::Call(c)) => {
            matches!(c.func.as_ref(), Expr::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "new"))
        }
        Some(Expr::Macro(m)) => {
            m.mac.path.segments.last().is_some_and(|s| s.ident == "vec") && m.mac.tokens.to_string().trim().is_empty()
        }
        _ => false,
    };
    if !empty {
        return Err(format!(
            "the accumulator `{acc}` must be initialised to an empty collection directly before the loop — the rewrite cannot preserve prior contents"
        ));
    }
    Ok(())
}

/// The fn ENDS with the accumulator — the move preserves its only reader.
fn pipeline_tail_ok(stmts: &[Stmt], acc: &str) -> Result<(), String> {
    let tail_ok = match stmts.last() {
        Some(Stmt::Expr(Expr::Path(p), _)) => p.path.segments.len() == 1 && p.path.segments[0].ident == acc,
        Some(Stmt::Expr(Expr::Return(r), _)) => match r.expr.as_deref() {
            Some(Expr::Path(p)) => p.path.segments.len() == 1 && p.path.segments[0].ident == acc,
            _ => false,
        },
        _ => false,
    };
    if !tail_ok {
        return Err(format!(
            "the function does not end with `{acc}` — the rewrite moves its binding"
        ));
    }
    Ok(())
}

/// No mid-body read of the accumulator outside the loop and tail.
fn pipeline_no_mid_read(stmts: &[Stmt], init_idx: usize, loop_idx: usize, acc: &str) -> Result<(), String> {
    struct ReadsAcc<'x> {
        acc: &'x str,
        found: bool,
    }
    impl syn::visit::Visit<'_> for ReadsAcc<'_> {
        fn visit_expr_path(&mut self, node: &syn::ExprPath) {
            if node.path.segments.len() == 1 && node.path.segments[0].ident == self.acc {
                self.found = true;
            }
        }
    }
    for (i, s) in stmts.iter().enumerate() {
        if i == init_idx || i == loop_idx || i == stmts.len() - 1 {
            continue;
        }
        if matches!(s, Stmt::Expr(Expr::ForLoop(_), _)) {
            continue;
        }
        let mut v = ReadsAcc { acc, found: false };
        v.visit_stmt(s);
        if v.found {
            return Err(format!(
                "the body reads `{acc}` outside the loop — the rewrite moves its binding"
            ));
        }
    }
    Ok(())
}

/// `loop-pipeline` (mechanical): the single-push for-loop at `line`
/// becomes the combinator that replaces it — `let <acc>: Vec<_> = <iter>
/// .iter().map(|<pat>| <arg>).collect();` (`.filter()` when the push sits
/// behind one if-filter). An indexed store becomes the same shape over
/// indices. Lossless text splice: the init + loop become one `let`, the
/// tail `return acc;`/`acc` keeps reading the new binding. Returns
/// Err(why) when the shape is not losslessly rewritable.
///
/// The detector calls `loop_pipeline_fixable` (same contract, bool instead
/// of the rewrite) before advertising `fix: loop-pipeline` — an
/// unfixable loop gets the finding WITHOUT the directive (offer equals
/// fix: review-bot).
pub fn loop_pipeline_fixable(source: &str, _file: &str, line: usize) -> bool {
    let Ok(file) = syn::parse_file(source) else {
        return false;
    };
    let Some(target) = enclosing_fn(&file, line) else {
        return false;
    };
    let stmts = &target.block.stmts;
    let Some(loop_idx) = stmts
        .iter()
        .position(|s| s.span().start().line == line && matches!(s, Stmt::Expr(Expr::ForLoop(_), _)))
    else {
        return false;
    };
    pipeline_plan(source, target, stmts, loop_idx, InitScope::Single).is_ok()
}

pub fn fix_loop_pipeline(source: &str, line: usize) -> Result<String, String> {
    let file = syn::parse_file(source).map_err(|_| "the file does not parse".to_string())?;
    let target = enclosing_fn(&file, line).ok_or_else(|| format!("no function contains line {line}"))?;
    // the loop AT the finding line — top-level statements only
    let stmts = &target.block.stmts;
    let loop_idx = stmts
        .iter()
        .position(|s| s.span().start().line == line && matches!(s, Stmt::Expr(Expr::ForLoop(_), _)))
        .ok_or_else(|| format!("no for-loop starts at line {line}"))?;
    let (init_idx, plan) = pipeline_plan(source, target, stmts, loop_idx, InitScope::Single)?;
    let replacement = if plan.indexed {
        // `out[i] = v` over `0..n` — the position IS the data; map over
        // indices and collect back into the same shape
        match &plan.filter {
            Some(cond) => format!(
                "let {}: Vec<_> = {}.filter(|{}| {}).map(|{}| {}).collect();",
                plan.acc, plan.iter, plan.pat, cond, plan.pat, plan.push_arg
            ),
            None => format!(
                "let {}: Vec<_> = {}.map(|{}| {}).collect();",
                plan.acc, plan.iter, plan.pat, plan.push_arg
            ),
        }
    } else {
        match &plan.filter {
            Some(cond) => format!(
                "let {}: Vec<_> = {}.iter().filter(|{}| {}).map(|{}| {}).collect();",
                plan.acc, plan.iter, plan.pat, cond, plan.pat, plan.push_arg
            ),
            None => format!(
                "let {}: Vec<_> = {}.iter().map(|{}| {}).collect();",
                plan.acc, plan.iter, plan.pat, plan.push_arg
            ),
        }
    };
    // splice: replace [init start .. loop end] with the one `let` (the
    // init's own indent survives as part of the prefix — nothing to add)
    let splice_start = byte_offset(source, stmts[init_idx].span().start());
    let splice_end = byte_offset(source, stmts[loop_idx].span().end());
    let mut out = String::new();
    out.push_str(&source[..splice_start]);
    out.push_str(&replacement);
    out.push_str(&source[splice_end..]);
    Ok(out)
}

/// The detector calls `loop_sequence_fixable` (same contract, bool instead
/// of the rewrite) before advertising `fix: loop-sequence` on a shared /
/// feed verdict — an unfixable chain gets the finding WITHOUT the
/// directive (offer equals fix: review-bot).
pub fn loop_sequence_fixable(source: &str, _file: &str, fn_line: usize) -> bool {
    fix_loop_sequence(source, fn_line).is_ok()
}

pub fn fix_loop_sequence(source: &str, line: usize) -> Result<String, String> {
    let file = syn::parse_file(source).map_err(|_| "the file does not parse".to_string())?;
    let target = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Fn(f) if f.sig.ident.span().start().line == line => Some(f),
            _ => None,
        })
        .ok_or_else(|| format!("no function starts at line {line}"))?;
    let stmts = &target.block.stmts;
    // the fn must OPEN with `let mut <acc> = <empty>;` — the shared init
    // the rewrite preserves only the loops, not prior contents
    let first = stmts
        .iter()
        .position(|s| !matches!(s, Stmt::Item(_)))
        .ok_or_else(|| "the function body is empty — no shared accumulator chain".to_string())?;
    let Stmt::Local(init) = &stmts[first] else {
        return Err("the function does not open with the shared accumulator init".to_string());
    };
    let Pat::Ident(acc_pat) = &init.pat else {
        return Err("the shared accumulator binding is not a plain name".to_string());
    };
    let acc = acc_pat.ident.to_string();
    // every statement after the init must be a for-loop (no interleaving)
    // and every loop must plan against the SAME accumulator
    let mut comps = Vec::new();
    let mut last_loop = first;
    for (i, s) in stmts.iter().enumerate().skip(first + 1) {
        if matches!(s, Stmt::Item(_)) {
            continue;
        }
        if i == stmts.len() - 1 {
            // the tail `out` / `return out;` is the accumulator read the
            // rewrite preserves — not a chain member
            continue;
        }
        let Stmt::Expr(Expr::ForLoop(_), _) = s else {
            return Err(
                "the chain mixes loops and other statements — reorder so the loops are consecutive".to_string(),
            );
        };
        let (_, plan) = pipeline_plan(source, target, stmts, i, InitScope::Chain)?;
        if plan.acc != acc {
            return Err(format!(
                "the chain mixes accumulators (`{}` vs `{}`) — every loop must build the same empty-initialized collection",
                plan.acc, acc
            ));
        }
        if plan.indexed {
            return Err("an indexed store in the chain has no combinator concatenation".to_string());
        }
        let one = match &plan.filter {
            Some(cond) => format!(
                "{}.iter().filter(|{}| {}).map(|{}| {}).collect::<Vec<_>>()",
                plan.iter, plan.pat, cond, plan.pat, plan.push_arg
            ),
            None => format!(
                "{}.iter().map(|{}| {}).collect::<Vec<_>>()",
                plan.iter, plan.pat, plan.push_arg
            ),
        };
        comps.push(one);
        last_loop = i;
    }
    if comps.len() < 2 {
        return Err("fewer than 2 pipeline loops — not a sequence".to_string());
    }
    let replacement = format!("let {acc}: Vec<_> = {};", comps.join(" + "));
    let splice_start = byte_offset(source, stmts[first].span().start());
    let splice_end = byte_offset(source, stmts[last_loop].span().end());
    let mut out = String::new();
    out.push_str(&source[..splice_start]);
    out.push_str(&replacement);
    out.push_str(&source[splice_end..]);
    Ok(out)
}

/// The hoist plan: the single accumulator the body appends to, plus the
/// body's own bindings (loop pattern + `let`s — temps the helper keeps).
struct HoistPlan {
    acc: String,
    push_args: Vec<proc_macro2::Span>,
}
/// The single accumulator the body pushes to, plus the push-receiver
/// paths (a mid-build READ check must not mistake a receiver for a read).
/// Any other mutating receiver on outer state refuses with its name.
// lucidlint: ignore complexity the push scan is one visitor — match-arm count, not branching
fn hoist_push_accum(
    body: &[Stmt],
    bound: &std::collections::HashSet<String>,
) -> Result<(String, Vec<*const syn::ExprPath>, Vec<proc_macro2::Span>), String> {
    struct PushScan<'x> {
        bound: &'x std::collections::HashSet<String>,
        acc: Option<String>,
        receivers: Vec<*const syn::ExprPath>,
        push_args: Vec<proc_macro2::Span>,
        decline: Option<String>,
    }
    impl syn::visit::Visit<'_> for PushScan<'_> {
        fn visit_expr_method_call(&mut self, node: &syn::ExprMethodCall) {
            let method = node.method.to_string();
            let outer_base = match node.receiver.as_ref() {
                syn::Expr::Path(p)
                    if p.path.segments.len() == 1 && !self.bound.contains(&p.path.segments[0].ident.to_string()) =>
                {
                    Some(p.path.segments[0].ident.to_string())
                }
                _ => None,
            };
            if let Some(base) = outer_base {
                if crate::rustloops::MUTATING_METHODS.contains(&method.as_str()) {
                    if method != "push" {
                        self.decline = Some(format!(
                            "`{method}` accumulation (`{base}`) keeps the loop — the hoist rewrite only expresses push"
                        ));
                        return;
                    }
                    match &self.acc {
                        None => self.acc = Some(base),
                        Some(a) if *a == base => {}
                        Some(a) => {
                            self.decline = Some(format!(
                                "the body accumulates BOTH `{a}` and `{base}` — the hoist rewrite needs one accumulator"
                            ));
                            return;
                        }
                    }
                    if let syn::Expr::Path(p) = node.receiver.as_ref() {
                        self.receivers.push(p as *const syn::ExprPath);
                    }
                    if let Some(arg0) = node.args.first() {
                        self.push_args.push(arg0.span());
                    }
                }
            }
            syn::visit::visit_expr_method_call(self, node);
        }
    }
    let mut scan = PushScan {
        bound,
        acc: None,
        receivers: Vec::new(),
        push_args: Vec::new(),
        decline: None,
    };
    for s in body {
        syn::visit::Visit::visit_stmt(&mut scan, s);
        if scan.decline.is_some() {
            break;
        }
    }
    if let Some(d) = scan.decline {
        return Err(d);
    }
    let acc = scan
        .acc
        .ok_or_else(|| "no outer collection mutation found in the loop body".to_string())?;
    Ok((acc, scan.receivers, scan.push_args))
}

/// No mid-build READ of the accumulator except as a push receiver.
fn hoist_no_mid_read(body: &[Stmt], acc: &str, receivers: &[*const syn::ExprPath]) -> Result<(), String> {
    struct AccRead<'x> {
        acc: &'x str,
        receivers: &'x [*const syn::ExprPath],
        found: bool,
    }
    impl syn::visit::Visit<'_> for AccRead<'_> {
        fn visit_expr_path(&mut self, node: &syn::ExprPath) {
            if node.path.segments.len() == 1
                && node.path.segments[0].ident == self.acc
                && !self.receivers.contains(&(node as *const syn::ExprPath))
            {
                self.found = true;
            }
        }
    }
    let mut read = AccRead {
        acc,
        receivers,
        found: false,
    };
    for s in body {
        syn::visit::Visit::visit_stmt(&mut read, s);
        if read.found {
            break;
        }
    }
    if read.found {
        return Err(format!(
            "the body READS `{acc}` mid-build — a combinator has no partial state to read; refactor by hand"
        ));
    }
    Ok(())
}

/// No other outer-state WRITE besides the accumulator.
fn hoist_no_other_write(body: &[Stmt], bound: &std::collections::HashSet<String>, acc: &str) -> Result<(), String> {
    struct WriteScan<'x> {
        bound: &'x std::collections::HashSet<String>,
        acc: &'x str,
        bad: Option<String>,
    }
    impl syn::visit::Visit<'_> for WriteScan<'_> {
        fn visit_expr_assign(&mut self, node: &syn::ExprAssign) {
            let mut base = Vec::new();
            crate::rustloops::expr_base_path(&node.left, &mut base);
            for b in base {
                if b != self.acc && !self.bound.contains(&b) {
                    self.bad = Some(b);
                    return;
                }
            }
            syn::visit::visit_expr_assign(self, node);
        }
        fn visit_expr_binary(&mut self, node: &syn::ExprBinary) {
            if crate::rustscan::is_compound_op(&node.op) {
                let mut base = Vec::new();
                crate::rustloops::expr_base_path(&node.left, &mut base);
                for b in base {
                    if b != self.acc && !self.bound.contains(&b) {
                        self.bad = Some(b);
                        return;
                    }
                }
            }
            syn::visit::visit_expr_binary(self, node);
        }
    }
    let mut writes = WriteScan { bound, acc, bad: None };
    for s in body {
        syn::visit::Visit::visit_stmt(&mut writes, s);
        if writes.bad.is_some() {
            break;
        }
    }
    if let Some(b) = writes.bad {
        return Err(format!(
            "the body also writes `{b}` — the hoist rewrite only moves a single-accumulator loop"
        ));
    }
    Ok(())
}

/// The `loop-hoist` plan for the for-loop at `loop_idx`: every outer-state
/// write is an `acc.push(..)` on ONE accumulator, and the body never READS
/// that accumulator mid-build (a combinator has no partial state to read).
/// Refusals name the violation — the Python `_hoist_plan` contract.
fn hoist_plan(source: &str, stmts: &[Stmt], loop_idx: usize) -> Result<(HoistPlan, String, String), String> {
    let Stmt::Expr(Expr::ForLoop(f), _) = &stmts[loop_idx] else {
        return Err("the statement at the finding line is not a for-loop".to_string());
    };
    let Pat::Ident(pi) = &*f.pat else {
        return Err("the loop target must be a simple name to become the helper's parameter".to_string());
    };
    let target = pi.ident.to_string();
    if target == "_" {
        return Err("the loop binds `_` — there is no item name for the helper's parameter".to_string());
    }
    // the body's own bindings: the loop pattern plus every `let` in the
    // body (nested `let`s via the visitor — a temp is a temp at any depth)
    struct Lets(std::collections::HashSet<String>);
    impl syn::visit::Visit<'_> for Lets {
        fn visit_local(&mut self, node: &syn::Local) {
            pat_bindings(&node.pat, &mut self.0);
        }
        fn visit_item_fn(&mut self, node: &syn::ItemFn) {
            for p in &node.sig.inputs {
                if let FnArg::Typed(t) = p {
                    pat_bindings(&t.pat, &mut self.0);
                }
            }
        }
    }
    let mut bound = std::collections::HashSet::new();
    {
        let mut v = Vec::new();
        crate::rustloops::pat_idents(&f.pat, &mut v);
        bound.extend(v);
    }
    for s in &f.body.stmts {
        let mut l = Lets(std::collections::HashSet::new());
        syn::visit::Visit::visit_stmt(&mut l, s);
        bound.extend(l.0);
    }
    let (acc, receivers, push_args) = hoist_push_accum(&f.body.stmts, &bound)?;
    hoist_no_mid_read(&f.body.stmts, &acc, &receivers)?;
    hoist_no_other_write(&f.body.stmts, &bound, &acc)?;
    let iter = span_text(source, f.expr.span())?;
    Ok((HoistPlan { acc, push_args }, target, iter))
}

/// A name absent from the file — the helper-local and the flatten variable
/// must not shadow an existing name (the Python `_fresh_loop_name`
/// contract).
fn fresh_name(source: &str, base: &str) -> String {
    let mut candidate = base.to_string();
    let mut i = 2;
    while source.contains(&candidate) {
        candidate = format!("{base}_{i}");
        i += 1;
    }
    candidate
}

/// Retarget push receivers in place: `acc.push` -> `<local>.push`.
fn hoist_retarget_pushes(acc: &str, helper_local: &str, moved: &mut String) {
    let needle = format!("{acc}.push");
    let replacement = format!("{helper_local}.push");
    let mut out_moved = String::with_capacity(moved.len());
    let mut rest = moved.as_str();
    while let Some(pos) = rest.find(&needle) {
        // the char before must not be an ident char (no `xacc.push`)
        let ok = pos == 0
            || !rest[..pos]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        out_moved.push_str(&rest[..pos]);
        out_moved.push_str(if ok { &replacement } else { &needle });
        rest = &rest[pos + needle.len()..];
    }
    out_moved.push_str(rest);
    *moved = out_moved;
}

/// The empty `let mut <acc>` DIRECTLY before the loop (attributes may sit
/// between) — the hoist cannot preserve prior contents.
fn hoist_init_index(stmts: &[Stmt], loop_idx: usize, acc: &str) -> Result<usize, String> {
    let mut init_idx = loop_idx;
    loop {
        if init_idx == 0 {
            return Err(format!(
                "the accumulator `{acc}` must be initialised to an empty collection directly before the loop — the hoist cannot preserve prior contents"
            ));
        }
        init_idx -= 1;
        match &stmts[init_idx] {
            Stmt::Item(_) => {}
            Stmt::Local(l) => match &l.pat {
                Pat::Ident(lp) if lp.ident == *acc => return Ok(init_idx),
                Pat::Type(pt) => match pt.pat.as_ref() {
                    Pat::Ident(lp) if lp.ident == *acc => return Ok(init_idx),
                    Pat::Ident(lp) => {
                        return Err(format!(
                            "the statement before the loop binds `{}` — the hoist needs `let mut {acc}` directly above",
                            lp.ident
                        ));
                    }
                    _ => {
                        return Err(format!(
                            "the statement before the loop is not `let mut {acc}` — the hoist cannot preserve prior contents"
                        ));
                    }
                },
                Pat::Ident(lp) => {
                    return Err(format!(
                        "the statement before the loop binds `{}` — the hoist needs `let mut {acc}` directly above",
                        lp.ident
                    ));
                }
                _ => {
                    return Err(format!(
                        "the statement before the loop is not `let mut {acc}` — the hoist cannot preserve prior contents"
                    ));
                }
            },
            _ => {
                return Err(format!(
                    "the accumulator `{acc}` must be initialised to an empty collection directly before the loop — the hoist cannot preserve prior contents"
                ));
            }
        }
    }
}

/// The helper item for the hoist: the Option shape wraps each push
/// arg in `Some(..)` with a `None` fallthrough (conditional push); the
/// direct shape returns the accumulator local.
/// The helper's item type from the accumulator's `Vec<T>` annotation —
/// guessing `_` never compiles (E0282/E0121), so a missing annotation
/// refuses with its reason.
fn hoist_item_ty(
    source: &str,
    stmts: &[Stmt],
    init_idx: usize,
    acc: &str,
    target_pat: &str,
) -> Result<(String, String), String> {
    let Stmt::Local(init) = &stmts[init_idx] else {
        return Err("the shared accumulator init is not a plain let binding".to_string());
    };
    let init_ty = match &init.pat {
        Pat::Type(pt) => {
            let ts = byte_offset(source, pt.ty.span().start());
            let te = byte_offset(source, pt.ty.span().end());
            source[ts..te].trim().to_string()
        }
        _ => {
            return Err(format!(
                "the accumulator `{acc}` needs a `Vec<T>` type annotation — the helper's signature cannot be inferred without it"
            ));
        }
    };
    let item_ty = init_ty
        .strip_prefix("Vec<")
        .and_then(|s| s.strip_suffix('>'))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!("the accumulator `{acc}` needs a `Vec<T>` type annotation — the helper's signature cannot be inferred without it")
        })?;
    // the pattern text (`x`, `*x`, `&x`) decides the call-site deref, not
    // the helper param (always the plain binding)
    let param = target_pat.trim_start_matches(['&', '*']).to_string();
    Ok((item_ty.to_string(), param))
}

struct HelperSig {
    helper_name: String,
    param: String,
    item_ty: String,
}

struct HelperBody {
    moved: String,
    helper_local: String,
    conditional: bool,
}

fn hoist_helper_text(source: &str, sig: &HelperSig, body: &HelperBody, push_args: &[proc_macro2::Span]) -> String {
    let HelperSig {
        helper_name,
        param,
        item_ty,
    } = sig;
    let HelperBody {
        moved,
        helper_local,
        conditional,
    } = body;
    if !conditional {
        return format!("fn {helper_name}({param}: {item_ty}) -> {item_ty} {{\n{moved}    {helper_local}\n}}\n\n");
    }
    // Option shape: wrap each push arg in Some(..), append `None` as
    // the fallthrough, return the Option. The arg text comes from the
    // push-call spans the plan recorded — syn spans, not a paren scan, so
    // nested calls and string literals (`format!("({})", x)`) slice exactly.
    let needle = format!("{helper_local}.push(");
    let mut rebuilt = String::with_capacity(moved.len() + 16);
    let mut rest: &str = moved;
    let mut spans = push_args.iter();
    while let Some(pos) = rest.find(&needle) {
        rebuilt.push_str(&rest[..pos]);
        let arg = match spans.next() {
            Some(span) => span_text(source, *span)
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| String::new()),

            // span/statement drift (should not happen — the spans were
            // recorded from this same body): fall back to no rewrite
            None => {
                rebuilt.push_str(&rest[pos..]);
                rest = "";
                break;
            }
        };
        rebuilt.push_str(&format!("return Some({arg});"));
        // skip past this push statement: the next `;` ends it
        let tail_start = pos + needle.len();
        let semi = rest[tail_start..]
            .find(';')
            .map(|i| tail_start + i + 1)
            .unwrap_or(rest.len());
        rest = &rest[semi..];
    }
    rebuilt.push_str(rest);
    format!("fn {helper_name}({param}: {item_ty}) -> Option<{item_ty}> {{\n{rebuilt}    None\n}}\n\n")
}

/// `loop-hoist` (mechanical): the single-accumulator for-loop at `line`
/// whose body computes more than it pushes becomes a per-item helper + a
/// flattening combinator. The body moves into the helper UNCHANGED except
/// for the retargeted push receiver (see `hoist_retarget_pushes`).
///
/// The detector calls `loop_hoist_fixable_at` (same contract, bool instead
/// of the rewrite) before advertising `fix: loop-hoist` — an unhoistable
/// body gets the finding WITHOUT the directive (offer equals fix:
/// review-bot). The name is NOT part of fixability: any name works once
/// the shape holds, so the probe passes a placeholder.
pub fn loop_hoist_fixable_at(source: &str, _file: &str, line: usize) -> bool {
    fix_loop_hoist(source, line, "probe").is_ok()
}

pub fn fix_loop_hoist(source: &str, line: usize, name: &str) -> Result<String, String> {
    if name.trim().is_empty() {
        return Err(
            "the hoisted helper needs a semantic name — pass --fix-name <Name> (a domain noun for what one item contributes)".to_string(),
        );
    }
    let helper_name = if name.starts_with('_') {
        name.to_string()
    } else {
        format!("_{name}")
    };
    let file = syn::parse_file(source).map_err(|_| "the file does not parse".to_string())?;
    let target = enclosing_fn(&file, line).ok_or_else(|| format!("no function contains line {line}"))?;
    let stmts = &target.block.stmts;
    let loop_idx = stmts
        .iter()
        .position(|s| s.span().start().line == line && matches!(s, Stmt::Expr(Expr::ForLoop(_), _)))
        .ok_or_else(|| format!("no for-loop starts at line {line}"))?;
    let (plan, target_pat, iter) = hoist_plan(source, stmts, loop_idx)?;
    let acc = &plan.acc;
    let init_idx = hoist_init_index(stmts, loop_idx, acc)?;
    // a fresh helper-local that shadows nothing in the file
    let helper_local = fresh_name(source, "val");
    let Stmt::Expr(Expr::ForLoop(f), _) = &stmts[loop_idx] else {
        return Err("the statement at the finding line is not a for-loop".to_string());
    };
    let body_start = byte_offset(source, f.body.brace_token.span.open().start()) + 1;
    let body_end = byte_offset(source, f.body.brace_token.span.close().end()) - 1;
    let body_text = source[body_start..body_end].to_string();
    let mut moved = body_text;
    hoist_retarget_pushes(acc, &helper_local, &mut moved);
    let (item_ty, param) = hoist_item_ty(source, stmts, init_idx, acc, &target_pat)?;
    // the body's push ARGUMENT decides value-vs-Option: a conditional push
    // (push behind if, or two push sites) means some items contribute
    // nothing — the helper returns Option<T>. An unconditional single push
    // returns T directly.
    let push_count = moved.matches(&format!("{helper_local}.push")).count();
    let conditional = push_count != 1
        || matches!(
            &stmts[loop_idx],
            Stmt::Expr(Expr::ForLoop(f), _)
                if !matches!(f.body.stmts.as_slice(), [_])
        );
    let helper = hoist_helper_text(
        source,
        &HelperSig {
            helper_name: helper_name.clone(),
            param: param.clone(),
            item_ty,
        },
        &HelperBody {
            moved,
            helper_local,
            conditional,
        },
        &plan.push_args,
    );
    // the loop becomes the combinator over the helper: filter_map for the
    // Option shape, map for the direct shape
    let new_let = if conditional {
        format!("let {acc}: Vec<_> = {iter}.iter().filter_map(|{param}| {helper_name}({param})).collect();")
    } else {
        format!("let {acc}: Vec<_> = {iter}.iter().map(|{param}| {helper_name}({param})).collect();")
    };
    // splice 1: replace [init start .. loop end] with the new let
    let splice_start = byte_offset(source, stmts[init_idx].span().start());
    let splice_end = byte_offset(source, stmts[loop_idx].span().end());
    let mut out = String::new();
    out.push_str(&source[..splice_start]);
    out.push_str(&new_let);
    out.push_str(&source[splice_end..]);
    // splice 2: insert the helper before the target fn
    let fn_start = byte_offset(&out, target.span().start());
    let mut final_out = String::new();
    final_out.push_str(&out[..fn_start]);
    final_out.push_str(&helper);
    final_out.push_str(&out[fn_start..]);
    Ok(final_out)
}

#[cfg(test)]
mod hoist_tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing fixture {name}"))
    }

    #[test]
    fn hoist_extracts_helper_and_flat_map() {
        let src = fixture("loop_hoist_rs_fix.rs");
        let out = fix_loop_hoist(&src, 3, "contribution").expect("fix applies");
        assert!(out.contains("fn _contribution("), "{out}");
        assert!(out.contains("filter_map"), "{out}");
        assert!(!out.contains("for x in xs"), "{out}");
        let file: syn::File = syn::parse_str(&out).expect("fixed source parses");
        assert_eq!(file.items.len(), 2);
    }

    #[test]
    fn hoist_requires_a_name_and_refuses_mid_read() {
        let src = "fn f(xs: &[u32]) -> Vec<u32> {\n    let mut out = Vec::new();\n    for x in xs {\n        out.push(*x);\n    }\n    out\n}\n";
        assert!(fix_loop_hoist(src, 3, "").is_err());
        let read = "fn f(xs: &[u32]) -> Vec<u32> {\n    let mut out = Vec::new();\n    for x in xs {\n        let n = out.len();\n        out.push(*x + n as u32);\n    }\n    out\n}\n";
        assert!(fix_loop_hoist(read, 3, "h").is_err());
    }

    #[test]
    fn hoist_option_arg_with_nested_parens_and_string() {
        // the paren scan this replaced would stop at the inner `)` of
        // `format!("({})", x)` — the span slice keeps the whole argument
        let src = "fn f(xs: &[u32]) -> Vec<String> {\n    let mut out: Vec<String> = Vec::new();\n    for x in xs {\n        let doubled = *x * 2;\n        if doubled > 1 {\n            out.push(format!(\"({})\", doubled));\n        }\n    }\n    out\n}\n";
        let out = fix_loop_hoist(src, 3, "label").expect("fix applies");
        assert!(out.contains("filter_map"), "{out}");
        assert!(out.contains("return Some(format!(\"({})\", doubled));"), "{out}");
        syn::parse_str::<syn::File>(&out).expect("fixed source parses");
    }
}

#[cfg(test)]
mod loop_tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing fixture {name}"))
    }

    #[test]
    fn pipeline_plain_push_becomes_map_collect() {
        let src = fixture("loop_pipeline_rs.rs");
        let out = fix_loop_pipeline(&src, 3).expect("fix applies");
        assert!(
            out.contains("let out: Vec<_> = xs.iter().map(|x| *x).collect();"),
            "{out}"
        );
        assert!(!out.contains("for x in xs"), "{out}");
        let file: syn::File = syn::parse_str(&out).expect("fixed source parses");
        assert_eq!(file.items.len(), 1);
    }

    #[test]
    fn pipeline_if_filter_becomes_filter_map() {
        let src = fixture("loop_pipeline_rs_filtered.rs");
        let out = fix_loop_pipeline(&src, 3).expect("fix applies");
        assert!(
            out.contains("let out: Vec<_> = xs.iter().filter(|x| *x > 0).map(|x| *x).collect();"),
            "{out}"
        );
    }

    #[test]
    fn sequence_shared_accumulator_becomes_concatenation() {
        let src = fixture("loop_sequence_rs_fix.rs");
        let out = fix_loop_sequence(&src, 1).expect("fix applies");
        assert!(out.contains("let out: Vec<_> = xs.iter().map(|x| *x).collect::<Vec<_>>() + xs.iter().map(|y| *y + 1).collect::<Vec<_>>();"), "{out}");
        assert!(!out.contains("for x in xs"), "{out}");
        let file: syn::File = syn::parse_str(&out).expect("fixed source parses");
        assert_eq!(file.items.len(), 1);
    }

    #[test]
    fn sequence_refuses_interleaved_statements() {
        let src = "fn f(xs: &[u32]) -> Vec<u32> {\n    let mut out = Vec::new();\n    for x in xs {\n        out.push(*x);\n    }\n    let n = out.len();\n    for y in xs {\n        out.push(*y);\n    }\n    out\n}\n";
        assert!(fix_loop_sequence(src, 1).is_err());
    }

    #[test]
    fn pipeline_fixes_a_loop_in_a_later_function() {
        // the review finding: the fixer resolved the FIRST fn, so a loop
        // in a later fn rewrote through the wrong accumulator (or refused
        // with "no for-loop at line N"). The finding line selects the
        // enclosing fn.
        let src = "fn first() -> u32 {\n    1\n}\n\nfn collect(xs: &[u32]) -> Vec<u32> {\n    let mut out = Vec::new();\n    for x in xs {\n        out.push(*x);\n    }\n    out\n}\n";
        let out = fix_loop_pipeline(src, 7).expect("fix applies in the later fn");
        assert!(
            out.contains("let out: Vec<_> = xs.iter().map(|x| *x).collect();"),
            "{out}"
        );
        assert!(out.contains("fn first() -> u32"), "{out}");
    }
}

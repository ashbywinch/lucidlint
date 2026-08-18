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

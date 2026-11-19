//! `dispatch-registry`: an if/elif chain of same-shape arms becomes one table
//! and one loop over it.

use syn::spanned::Spanned;
use syn::{Expr, Item, Stmt};

use super::rule_table::{block_ends_with_return, param_is_str};
use super::text::FixText;

/// A Rust dispatch chain (`if sel == "a" { ... } if sel == "b" { ... }`) is
/// most lucidly a `match` — Rust's idiomatic dispatch table. Lossless text
/// splice: the if-conditions become match arms, the trailing fallback the
/// `_` arm (review: run_tool's Rust analog). Returns Err(why) when the shape
/// is not a clean chain.
/// Parse contiguous dispatch arms from a statement list, starting at `first_if`.
/// Returns (selector, arms_text, lits, next_idx) or Err(why).
pub(super) fn parse_dispatch_arms(
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
        let block = FixText::span_text(source, i.then_branch.span())?;
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
        let start = FixText::byte_offset(source, stmts[idx].span().start());
        let end = FixText::byte_offset(source, stmts[stmts.len() - 1].span().end());
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
    let splice_start = FixText::byte_offset(source, stmts[first_if].span().start());
    let splice_end = FixText::byte_offset(source, stmts[stmts.len() - 1].span().end());
    let mut out = String::new();
    out.push_str(&source[..splice_start]);
    out.push_str(&match_text);
    out.push_str(&source[splice_end..]);
    Ok(out)
}

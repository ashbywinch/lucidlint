//! `loop-sequence`: consecutive pipeline loops over one accumulator chain into a
//! single collect.

use syn::spanned::Spanned;
use syn::{Expr, Item, Pat, Stmt};

use super::loop_pipeline::{InitScope, LoopPipeline};
use super::text::FixText;

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
        let (_, plan) = LoopPipeline::plan(source, target, stmts, i, InitScope::Chain)?;
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
    let splice_start = FixText::byte_offset(source, stmts[first].span().start());
    let splice_end = FixText::byte_offset(source, stmts[last_loop].span().end());
    let mut out = String::new();
    out.push_str(&source[..splice_start]);
    out.push_str(&replacement);
    out.push_str(&source[splice_end..]);
    Ok(out)
}

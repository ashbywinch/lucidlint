//! `extract-method` for Rust: cut a contiguous statement block into a helper,
//! passing the seam's free variables as parameters and its outputs back.

use syn::spanned::Spanned;
use syn::{FnArg, Ident, Item, ItemFn, Pat, Stmt};

use super::seam::Seam;
use super::text::FixText;

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
            if seam.iter().any(|s| Seam::is_control_flow(s)) {
                continue;
            }
            let after: Vec<&Stmt> = stmts[start + len..].to_vec();
            if let Some(free) = Seam::seam_analysis(seam, &after, &params) {
                return apply(source, target, seam, &free, name).ok_or_else(|| {
                    "the seam's free variables are not a subset of the function's parameters".to_string()
                });
            }
        }
    }
    Err("no self-contained seam exists: every candidate has control flow, reads a value written in the seam (out-variable), or calls a method/`self`".to_string())
}

pub(super) fn apply(source: &str, target: &ItemFn, seam: &[&Stmt], free: &[Ident], name: &str) -> Option<String> {
    let first = seam.first()?.span().start();
    let last = seam.last()?.span().end();
    let seam_start = FixText::byte_offset(source, first);
    let seam_end = FixText::byte_offset(source, last);
    // the target fn's start — insert the helper just before it
    let fn_start = FixText::byte_offset(source, target.span().start());
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
        let ts = FixText::byte_offset(source, ty_span.start());
        let te = FixText::byte_offset(source, ty_span.end());
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
        let gs = FixText::byte_offset(source, target.sig.generics.span().start());
        let ge = FixText::byte_offset(source, target.sig.generics.span().end());
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

//! `loop-pipeline`: one pure push loop becomes a combinator chain, and the plan
//! decides when that is safe (one accumulator, no mid-build read, empty init).

use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Expr, ItemFn, Pat, Stmt};

use super::text::FixText;

/// One loop's pipeline plan: the accumulator, the shape (pure-push keeps
/// the borrowed item, an if-filter narrows it, an index store needs the
/// position), and the source slices the combinator rewrite needs.
pub(super) struct PipelinePlan {
    pub(super) acc: String,
    pub(super) filter: Option<String>,
    pub(super) indexed: bool,
    pub(super) push_arg: String,
    pub(super) pat: String,
    pub(super) iter: String,
}

/// The loop's item binding: a plain single name (never `_` or
/// destructuring — there is no name to rebind in the closure parameter).
/// Returns the binding plus whether the push sits behind an if-filter.
// lucidlint: ignore complexity the two refuses are the contract — one shape, two names for failure
pub(super) fn pipeline_loop_target(f: &syn::ExprForLoop) -> Result<(String, bool), String> {
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
pub(super) enum InitScope {
    Single,
    Chain,
}

pub(super) fn pipeline_plan(
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
        [Stmt::Expr(Expr::If(i), _)] => Some(FixText::span_text(source, i.cond.span())?),
        _ => None,
    };
    let push_arg = match core {
        Stmt::Expr(Expr::MethodCall(m), Some(_)) => FixText::span_text(source, m.args[0].span())?,
        Stmt::Expr(Expr::Assign(a), Some(_)) => FixText::span_text(source, a.right.span())?,
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
    let iter = FixText::span_text(source, f.expr.span())?;
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
pub(super) fn pipeline_init_index(
    stmts: &[Stmt],
    loop_idx: usize,
    scope: InitScope,
    acc: &str,
) -> Result<usize, String> {
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
pub(super) fn pipeline_empty_init(stmts: &[Stmt], init_idx: usize, acc: &str) -> Result<(), String> {
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
pub(super) fn pipeline_tail_ok(stmts: &[Stmt], acc: &str) -> Result<(), String> {
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
pub(super) fn pipeline_no_mid_read(stmts: &[Stmt], init_idx: usize, loop_idx: usize, acc: &str) -> Result<(), String> {
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
        if is_loop_edge(i, init_idx, loop_idx, stmts.len()) {
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
    let Some(target) = FixText::enclosing_fn(&file, line) else {
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
    let target = FixText::enclosing_fn(&file, line).ok_or_else(|| format!("no function contains line {line}"))?;
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
    let splice_start = FixText::byte_offset(source, stmts[init_idx].span().start());
    let splice_end = FixText::byte_offset(source, stmts[loop_idx].span().end());
    let mut out = String::new();
    out.push_str(&source[..splice_start]);
    out.push_str(&replacement);
    out.push_str(&source[splice_end..]);
    Ok(out)
}

/// The statement is one of the loop's edges: the initialiser, the loop header,
/// or the trailing return — none of them moves into the helper.
pub(super) fn is_loop_edge(index: usize, init_idx: usize, loop_idx: usize, len: usize) -> bool {
    index == init_idx || index == loop_idx || index + 1 == len
}

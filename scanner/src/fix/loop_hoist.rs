//! `loop-hoist`: the per-item step of one long loop becomes a named helper that
//! returns the value, so the loop reads as what it builds.

use syn::spanned::Spanned;
use syn::{Expr, FnArg, Pat, Stmt};

use super::text::FixText;

/// The hoist plan: the single accumulator the body appends to, the push sites'
/// argument spans (the helper's return value), their RECEIVER spans (the
/// retarget rewrites those ranges) and their STATEMENT spans (the Option shape
/// replaces each statement by the return) — the shape decision counts the
/// receivers, never a text search over the body (a string literal naming the
/// accumulator would read as a second push site, and an argument containing a
/// `;` would end a statement scan early).
pub(super) struct HoistPlan {
    pub(super) acc: String,
    pub(super) push_args: Vec<proc_macro2::Span>,
    pub(super) receiver_spans: Vec<proc_macro2::Span>,
    pub(super) push_stmts: Vec<proc_macro2::Span>,
}

/// The single accumulator the body pushes to, plus the push-receiver
/// paths (a mid-build READ check must not mistake a receiver for a read).
/// Any other mutating receiver on outer state refuses with its name.
// lucidlint: ignore complexity the push scan is one visitor — match-arm count, not branching
pub(super) fn hoist_push_accum(
    body: &[Stmt],
    bound: &std::collections::HashSet<String>,
) -> Result<(HoistPlan, Vec<*const syn::ExprPath>), String> {
    struct PushScan<'x> {
        bound: &'x std::collections::HashSet<String>,
        acc: Option<String>,
        receivers: Vec<*const syn::ExprPath>,
        push_args: Vec<proc_macro2::Span>,
        receiver_spans: Vec<proc_macro2::Span>,
        push_stmts: Vec<proc_macro2::Span>,
        stmt_span: Option<proc_macro2::Span>,
        decline: Option<String>,
    }
    impl syn::visit::Visit<'_> for PushScan<'_> {
        fn visit_stmt(&mut self, node: &syn::Stmt) {
            // the innermost enclosing statement, so a push site's replacement
            // range is its own statement — never a needle search plus a `;` scan
            let prev = self.stmt_span;
            self.stmt_span = Some(node.span());
            syn::visit::visit_stmt(self, node);
            self.stmt_span = prev;
        }
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
                        self.receiver_spans.push(p.span());
                        if let Some(s) = self.stmt_span {
                            self.push_stmts.push(s);
                        }
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
        receiver_spans: Vec::new(),
        push_stmts: Vec::new(),
        stmt_span: None,
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
    Ok((
        HoistPlan {
            acc,
            push_args: scan.push_args,
            receiver_spans: scan.receiver_spans,
            push_stmts: scan.push_stmts,
        },
        scan.receivers,
    ))
}

/// No mid-build READ of the accumulator except as a push receiver.
pub(super) fn hoist_no_mid_read(body: &[Stmt], acc: &str, receivers: &[*const syn::ExprPath]) -> Result<(), String> {
    struct AccRead<'x> {
        acc: &'x str,
        receivers: &'x [*const syn::ExprPath],
        found: bool,
    }
    impl AccRead<'_> {
        fn is_accumulator_read(&self, node: &syn::ExprPath) -> bool {
            node.path.segments.len() == 1
                && node.path.segments[0].ident == self.acc
                && !self.receivers.contains(&(node as *const syn::ExprPath))
        }
    }

    impl syn::visit::Visit<'_> for AccRead<'_> {
        fn visit_expr_path(&mut self, node: &syn::ExprPath) {
            if self.is_accumulator_read(node) {
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
pub(super) fn hoist_no_other_write(
    body: &[Stmt],
    bound: &std::collections::HashSet<String>,
    acc: &str,
) -> Result<(), String> {
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
pub(super) fn hoist_plan(source: &str, stmts: &[Stmt], loop_idx: usize) -> Result<(HoistPlan, String, String), String> {
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
            FixText::pat_bindings(&node.pat, &mut self.0);
        }
        fn visit_item_fn(&mut self, node: &syn::ItemFn) {
            for p in &node.sig.inputs {
                if let FnArg::Typed(t) = p {
                    FixText::pat_bindings(&t.pat, &mut self.0);
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
    let (plan, receivers) = hoist_push_accum(&f.body.stmts, &bound)?;
    hoist_no_mid_read(&f.body.stmts, &plan.acc, &receivers)?;
    hoist_no_other_write(&f.body.stmts, &bound, &plan.acc)?;
    let iter = FixText::span_text(source, f.expr.span())?;
    Ok((plan, target, iter))
}

/// A name absent from the file — the helper-local and the flatten variable
/// must not shadow an existing name (the Python `_fresh_loop_name`
/// contract).
pub(super) fn fresh_name(source: &str, base: &str) -> String {
    let mut candidate = base.to_string();
    let mut i = 2;
    while source.contains(&candidate) {
        candidate = format!("{base}_{i}");
        i += 1;
    }
    candidate
}

/// Retarget the push receivers in place: `acc.push` -> `<local>.push`, by the
/// receiver SPANS the plan recorded. A text pass over the body would also
/// rewrite the needle inside a string literal or a comment (a message naming
/// the accumulator), and the shape decision would read that literal as a
/// second push site. A span that does not lie inside the body is a bug in the
/// plan, not a case to skip: refuse rather than rewrite the wrong range.
pub(super) fn hoist_retarget_pushes(
    moved: &mut String,
    body_start: usize,
    source: &str,
    receiver_spans: &[proc_macro2::Span],
    helper_local: &str,
) -> Result<(), String> {
    let mut ranges: Vec<(usize, usize)> = receiver_spans
        .iter()
        .map(|s| {
            (
                FixText::byte_offset(source, s.start()).saturating_sub(body_start),
                FixText::byte_offset(source, s.end()).saturating_sub(body_start),
            )
        })
        .collect();
    ranges.sort_unstable();
    for (start, end) in ranges.into_iter().rev() {
        if start >= end || end > moved.len() {
            return Err(format!(
                "a push receiver's span [{start}..{end}) does not lie inside the loop body ({} bytes) — refusing rather than rewriting the wrong range",
                moved.len()
            ));
        }
        moved.replace_range(start..end, helper_local);
    }
    Ok(())
}

/// The empty `let mut <acc>` DIRECTLY before the loop (attributes may sit
/// between) — the hoist cannot preserve prior contents.
pub(super) fn hoist_init_index(stmts: &[Stmt], loop_idx: usize, acc: &str) -> Result<usize, String> {
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
pub(super) fn hoist_item_ty(
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
            let ts = FixText::byte_offset(source, pt.ty.span().start());
            let te = FixText::byte_offset(source, pt.ty.span().end());
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

pub(super) struct HelperSig {
    helper_name: String,
    param: String,
    item_ty: String,
}

pub(super) struct HelperBody {
    moved: String,
    helper_local: String,
    conditional: bool,
}

/// `push_sites`: each push STATEMENT's moved-relative range, paired with its
/// argument's span. Spans, never a needle search plus a `;` scan: an argument
/// containing a `;` (a block or a closure) would end that scan mid-argument.
pub(super) fn hoist_helper_text(
    source: &str,
    sig: &HelperSig,
    body: &HelperBody,
    push_sites: &[(usize, usize, proc_macro2::Span)],
) -> String {
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
    // Option shape: wrap each push arg in Some(..), append `None` as the
    // fallthrough, return the Option. Each push statement's span becomes the
    // return, and the arg text slices from its own span — nested calls, string
    // literals (`format!("({})", x)`) and blocks with their own `;` all exact.
    let mut rebuilt = String::with_capacity(moved.len() + 16);
    let mut cursor = 0usize;
    for (start, end, arg_span) in push_sites {
        // a site already covered by an earlier range (a nested push in the same
        // statement) is left as it stands
        if *start < cursor || *end > moved.len() {
            continue;
        }
        rebuilt.push_str(&moved[cursor..*start]);
        let arg = FixText::span_text(source, *arg_span)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        rebuilt.push_str(&format!("return Some({arg});"));
        cursor = *end;
    }
    rebuilt.push_str(&moved[cursor..]);
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
    let target = FixText::enclosing_fn(&file, line).ok_or_else(|| format!("no function contains line {line}"))?;
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
    let body_start = FixText::byte_offset(source, f.body.brace_token.span.open().start()) + 1;
    let body_end = FixText::byte_offset(source, f.body.brace_token.span.close().end()) - 1;
    let body_text = source[body_start..body_end].to_string();
    let mut moved = body_text;
    hoist_retarget_pushes(&mut moved, body_start, source, &plan.receiver_spans, &helper_local)?;
    let (item_ty, param) = hoist_item_ty(source, stmts, init_idx, acc, &target_pat)?;
    // the body's push ARGUMENT decides value-vs-Option: a conditional push
    // (push behind if, or two push sites) means some items contribute
    // nothing — the helper returns Option<T>. An unconditional single push
    // returns T directly.
    let push_count = plan.receiver_spans.len();
    let conditional = push_count != 1
        || matches!(
            &stmts[loop_idx],
            Stmt::Expr(Expr::ForLoop(f), _)
                if !matches!(f.body.stmts.as_slice(), [_])
        );
    let mut push_sites: Vec<(usize, usize, proc_macro2::Span)> = plan
        .push_stmts
        .iter()
        .zip(&plan.push_args)
        .map(|(stmt, arg)| {
            (
                FixText::byte_offset(source, stmt.start()).saturating_sub(body_start),
                FixText::byte_offset(source, stmt.end()).saturating_sub(body_start),
                *arg,
            )
        })
        .collect();
    // the rebuild walks the sites ascending; a nested site (its range inside
    // the previous one) is then the only skip, and it is skipped on purpose
    push_sites.sort_by_key(|(start, _, _)| *start);
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
        &push_sites,
    );
    // the loop becomes the combinator over the helper: filter_map for the
    // Option shape, map for the direct shape
    let new_let = if conditional {
        format!("let {acc}: Vec<_> = {iter}.iter().filter_map(|{param}| {helper_name}({param})).collect();")
    } else {
        format!("let {acc}: Vec<_> = {iter}.iter().map(|{param}| {helper_name}({param})).collect();")
    };
    // splice 1: replace [init start .. loop end] with the new let
    let splice_start = FixText::byte_offset(source, stmts[init_idx].span().start());
    let splice_end = FixText::byte_offset(source, stmts[loop_idx].span().end());
    let mut out = String::new();
    out.push_str(&source[..splice_start]);
    out.push_str(&new_let);
    out.push_str(&source[splice_end..]);
    // splice 2: insert the helper before the target fn
    let fn_start = FixText::byte_offset(&out, target.span().start());
    let mut final_out = String::new();
    final_out.push_str(&out[..fn_start]);
    final_out.push_str(&helper);
    final_out.push_str(&out[fn_start..]);
    Ok(final_out)
}

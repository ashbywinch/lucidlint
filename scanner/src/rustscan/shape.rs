//! Function-shape classification: which of the house shapes a Rust function is
//! (dispatch, rule battery, single push, closures candidate), from its body alone.

use syn::visit::Visit;
use syn::{BinOp, FnArg, Pat, Signature};

/// The Rust function shapes — dispatch, rule battery, single push, closures —
/// read from one body: the classifier and the predicates its offer rests on.
pub(crate) struct RustShape;

/// The item count that makes a body a pipeline, and the closure candidate's
/// complexity and span bars.
const PIPELINE_ITEMS_MIN: usize = 3;
const CLOSURE_CC_MIN: u32 = 15;
const CLOSURE_SPAN_MIN: usize = 60;

impl RustShape {
    /// A one-statement placeholder body: empty, `return`, `return None`, `()` or
    /// a `let _ = ...` discard — a framework override stub, not a param-list
    /// smell (parity with the Python rule; review: urllib's redirect_request).
    pub(super) fn is_trivial_stub(stmts: &[syn::Stmt]) -> bool {
        if stmts.len() > 1 {
            return false;
        }
        match stmts.first() {
            None => true,
            Some(syn::Stmt::Expr(syn::Expr::Return(r), _)) => match r.expr.as_deref() {
                None => true,
                Some(syn::Expr::Path(p)) => p.path.is_ident("None"),
                _ => false,
            },
            Some(syn::Stmt::Local(l)) => matches!(&l.pat, syn::Pat::Wild(_)),
            _ => false,
        }
    }

    /// Classify a Rust function body's shape for the complexity message — parity
    /// with python_fn_shape: a dispatch chain (>= 3 if-expressions comparing the
    /// SAME selector to string literals) or a rule battery (>= 3 if-expressions
    /// pushing onto the SAME Vec) get shape-specific lucid guidance.
    /// The fn's typed-parameter names — the rule-battery classifier's
    /// fn-pointer capture boundary.
    pub(crate) fn param_names(sig: &Signature) -> std::collections::HashSet<String> {
        sig.inputs
            .iter()
            .filter_map(|a| match a {
                FnArg::Typed(t) => match t.pat.as_ref() {
                    Pat::Ident(pi) => Some(pi.ident.to_string()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    pub(super) fn fn_shape(body: &[syn::Stmt], sig: &syn::Signature) -> (&'static str, String) {
        // the OFFER must equal the FIX: a shape is only "dispatch"/"rules" when
        // the fix's own shape checks pass (parity with python_fn_shape)
        if let Some(sel) = Self::dispatch_shape(body, sig) {
            return ("dispatch", sel);
        }
        if let Some(acc) = Self::rule_battery_shape(body, &Self::param_names(sig)) {
            return ("rules", acc);
        }
        ("plain", String::new())
    }

    /// One Rust dispatch arm's selector when the condition is `sel == "lit"`.
    pub(super) fn arm_selector(i: &syn::ExprIf) -> Option<String> {
        use syn::Expr;
        let Expr::Binary(b) = i.cond.as_ref() else { return None };
        if !matches!(b.op, BinOp::Eq(_)) {
            return None;
        }
        let Expr::Path(p) = b.left.as_ref() else { return None };
        if p.path.segments.len() != 1 {
            return None;
        }
        if !matches!(
            b.right.as_ref(),
            Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(_),
                ..
            })
        ) {
            return None;
        }
        Some(p.path.segments[0].ident.to_string())
    }

    /// The FIXABLE Rust dispatch shape: >= 3 contiguous leading
    /// `if <sel> == "lit" { ... }` arms over ONE selector, each arm ending in a
    /// `return`, no else/else-if, unique literals, and a `&str` selector — the
    /// exact shape fix.rs's match rewrite compiles blind (offer equals fix).
    pub(super) fn dispatch_shape(body: &[syn::Stmt], sig: &syn::Signature) -> Option<String> {
        use syn::Expr;
        let first_if = body
            .iter()
            .position(|s| matches!(s, syn::Stmt::Expr(e, _) if matches!(e, Expr::If(_))))?;
        let mut selector: Option<String> = None;
        let mut n = 0usize;
        let mut lits: Vec<String> = Vec::new();
        for stmt in &body[first_if..] {
            let syn::Stmt::Expr(e, _) = stmt else { break };
            let Expr::If(i) = e else { break };
            let sel = Self::arm_selector(i)?;
            if i.else_branch.is_some() {
                return None; // an else/else-if would be deleted by the rewrite
            }
            // every arm must diverge — the match arms need no matching types
            let ends_in_return = matches!(
                i.then_branch.stmts.last(),
                Some(syn::Stmt::Expr(syn::Expr::Return(_), Some(_)))
            );
            if !ends_in_return {
                return None;
            }
            // duplicate literals -> unreachable-pattern compile error
            let Expr::Binary(b) = i.cond.as_ref() else { return None };
            let Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s), ..
            }) = b.right.as_ref()
            else {
                return None;
            };
            let lit = s.token().to_string();
            if lits.contains(&lit) {
                return None;
            }
            lits.push(lit);
            match &selector {
                None => selector = Some(sel),
                Some(s) if *s == sel => {}
                _ => return None,
            }
            n += 1;
        }
        if n < PIPELINE_ITEMS_MIN {
            return None;
        }
        let sel = selector?;
        if !Self::param_is_str(sig, &sel) {
            return None; // the match rewrite only compiles for `&str` selectors
        }
        Some(sel)
    }

    /// Is `name` a parameter declared `&str` (no lifetime)?
    pub(super) fn param_is_str(sig: &syn::Signature, name: &str) -> bool {
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

    /// The bare identifiers referenced in an expression (Rust side).
    pub(crate) fn expr_idents(e: &syn::Expr) -> Vec<String> {
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

    /// Is the block exactly one `acc.push(<lit>);` statement?
    pub(super) fn is_single_push(stmts: &[syn::Stmt], acc: &str) -> bool {
        use syn::Expr;
        if stmts.len() != 1 {
            return false;
        }
        let syn::Stmt::Expr(e, Some(_)) = &stmts[0] else {
            return false;
        };
        let Expr::MethodCall(m) = e else {
            return false;
        };
        m.method == "push"
            && matches!(m.receiver.as_ref(), Expr::Path(p)
            if p.path.segments.len() == 1 && p.path.segments[0].ident == acc)
            && m.args.len() == 1
            && matches!(
                &m.args[0],
                Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(_),
                    ..
                })
            )
    }

    /// The FIXABLE Rust rule battery: `let mut <acc> = vec![];`, then >= 3
    /// contiguous `if <cond> { <acc>.push("msg"); }` arms (the block is exactly
    /// the push; the cond reads only the fn's params — fn-pointer conditions
    /// cannot capture), and the tail is the <acc> expression. Mirrors fix.rs.
    pub(super) fn rule_battery_shape(body: &[syn::Stmt], params: &std::collections::HashSet<String>) -> Option<String> {
        use syn::Expr;
        let Some(syn::Stmt::Local(init)) = body.first() else {
            return None;
        };
        let syn::Pat::Ident(acc_pat) = &init.pat else {
            return None;
        };
        let acc = acc_pat.ident.to_string();
        let mut n = 0usize;
        for stmt in &body[1..] {
            let syn::Stmt::Expr(e, _) = stmt else { break };
            let Expr::If(i) = e else { break };
            if !Self::is_single_push(&i.then_branch.stmts, &acc) {
                return None;
            }
            for name in Self::expr_idents(&i.cond) {
                if !params.contains(&name) {
                    return None;
                }
            }
            n += 1;
        }
        if n < PIPELINE_ITEMS_MIN {
            return None;
        }
        let syn::Stmt::Expr(te, _) = body.last()? else {
            return None;
        };
        let Expr::Path(tp) = te else {
            return None;
        };
        if tp.path.segments.len() != 1 || tp.path.segments[0].ident != acc {
            return None;
        }
        Some(acc)
    }

    /// The expression discards its value: not a compound assignment, not control
    /// flow, and not one of the harmless statement forms.
    pub(super) fn is_discarded_expression(compound_assign: bool, control: bool, harmless: bool) -> bool {
        !compound_assign && !control && !harmless
    }

    /// The function is a closures candidate: nested definitions plus size.
    pub(super) fn is_closures_shape(inner: u32, cc: u32, span: usize) -> bool {
        inner >= 2 && (cc >= CLOSURE_CC_MIN || span >= CLOSURE_SPAN_MIN)
    }
}

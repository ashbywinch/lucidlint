//! The seam: what a candidate block needs from its surroundings — the names it
//! binds, the values it reads and writes, and whether the flow leaves it.

use syn::visit::Visit;
use syn::{FnArg, Ident, ItemFn, Stmt};

use super::text::FixText;

/// The seam: what a candidate block needs from its surroundings.
pub struct Seam;

impl Seam {
    /// Does `stmt` contain a control-flow exit ANYWHERE in its subtree? A nested
    /// `return` inside an `if` still changes which function returns.
    pub(super) fn is_control_flow(stmt: &Stmt) -> bool {
        /// The one walk this question needs: any return/break/continue.
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
        let mut v = FlowExitVisitor { found: false };
        v.visit_stmt(stmt);
        v.found
    }

    /// The names `let`-bound directly in the seam (its locals).
    pub(super) fn seam_bindings(stmts: &[&Stmt]) -> std::collections::HashSet<String> {
        /// Collect `let`/loop/closure/match bindings in a statement set.
        struct BindCollector<'a> {
            bound: &'a mut std::collections::HashSet<String>,
        }
        impl Visit<'_> for BindCollector<'_> {
            fn visit_local(&mut self, node: &syn::Local) {
                FixText::pat_bindings(&node.pat, self.bound);
            }
            fn visit_expr_for_loop(&mut self, node: &syn::ExprForLoop) {
                FixText::pat_bindings(&node.pat, self.bound);
                syn::visit::visit_expr_for_loop(self, node);
            }
            fn visit_expr_match(&mut self, node: &syn::ExprMatch) {
                for arm in &node.arms {
                    FixText::pat_bindings(&arm.pat, self.bound);
                }
                syn::visit::visit_expr_match(self, node);
            }
            fn visit_item_fn(&mut self, node: &ItemFn) {
                // nested fn is a new scope — its params bind here, its body doesn't
                // contribute to ours; skip the body
                for p in &node.sig.inputs {
                    if let FnArg::Typed(t) = p {
                        FixText::pat_bindings(&t.pat, self.bound);
                    }
                }
            }
        }
        let mut bound = std::collections::HashSet::new();
        for s in stmts {
            BindCollector { bound: &mut bound }.visit_stmt(s);
        }
        bound
    }

    /// The seam's free variables — the parameters it reads — or None when a
    /// read cannot be given a type (self, an unknown name).
    fn seam_free(
        stmts: &[&Stmt],
        bound: &std::collections::HashSet<String>,
        params: &std::collections::HashSet<String>,
    ) -> Option<Vec<Ident>> {
        /// The seam's reads: a name it neither binds nor receives is unknown.
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
        let mut free: Vec<Ident> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut ok = true;
        for s in stmts {
            SeamCollector {
                bound,
                params,
                free: &mut free,
                seen: &mut seen,
                ok: &mut ok,
            }
            .visit_stmt(s);
        }
        ok.then_some(free)
    }

    /// Does an after-statement read a seam-local the seam computed?
    fn reads_seam_local(after: &[&Stmt], seam_writes: &std::collections::HashSet<String>) -> bool {
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
        let mut out = false;
        for s in after {
            OutVarCollector {
                seam_writes,
                out: &mut out,
            }
            .visit_stmt(s);
        }
        out
    }

    /// Does an after-statement read one of the seam's free names?
    fn reads_after(after: &[&Stmt], names: &std::collections::HashSet<String>) -> bool {
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
        let mut out = false;
        for s in after {
            AfterReadCollector { names, out: &mut out }.visit_stmt(s);
        }
        out
    }

    /// The seam's free variables (fn params it reads) and whether it is safe.
    pub(super) fn seam_analysis(stmts: &[&Stmt], after: &[&Stmt], params: &[Ident]) -> Option<Vec<Ident>> {
        let pset: std::collections::HashSet<String> = params.iter().map(|i| i.to_string()).collect();
        let bound = Self::seam_bindings(stmts);
        let free = Self::seam_free(stmts, &bound, &pset)?;
        // out-vars: any seam-local read in the after-statements
        if Self::reads_seam_local(after, &bound) {
            return None;
        }
        // a free PARAM read in the after-statements would be MOVED into the
        // helper by value — the after-read becomes use-of-moved-value; refuse
        let free_set: std::collections::HashSet<String> = free.iter().map(|i| i.to_string()).collect();
        if !free_set.is_empty() && Self::reads_after(after, &free_set) {
            return None;
        }
        // a param SHADOWED by a seam-local binding that the seam also READS:
        // the read may be of the param (before the shadow) — the binding set is
        // order-insensitive, so the helper would get no argument and reference
        // an undefined name; refuse (conservative, offer-equals-fix)
        let seam_reads = Self::seam_read_names(stmts);
        for name in &bound {
            if pset.contains(name) && seam_reads.contains(name) {
                return None;
            }
        }
        Some(free)
    }

    /// All bare identifiers read in a statement run.
    pub(super) fn seam_read_names(stmts: &[&Stmt]) -> std::collections::HashSet<String> {
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
}

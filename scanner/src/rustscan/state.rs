//! The per-file walk: `RsState` carries what one file's scan accumulates, and
//! its `syn::visit::Visit` impl dispatches each node to the family that owns it.

use std::collections::HashMap;

use crate::finding::Finding;
use crate::FnCc;

use super::ast::{
    attrs_of, block_refs_self, collect_type_idents, has_attr, has_cfg_test, impl_self_name, inner_count,
    is_assert_macro, is_compound_op, is_dbg_macro, is_diverge_stmt, is_unwrap_expect, return_values, span_lines,
    static_type_ident, suffix_of, tokens_contain_ident, use_paths,
};
use super::decisions::{
    names_a_decision, rs_bool_operands, rs_condition_message, rs_condition_nouns, rs_expr_text,
    rs_names_a_type_or_constant,
};
use super::fakefs::fakefs_hit;
use super::shape::{is_closures_shape, is_discarded_expression, is_trivial_rust_stub, rust_fn_shape};
use super::skeleton::rs_skeleton;
use super::{RsStruct, RustScan};

use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Arm, Attribute, BinOp, Block, Expr, ExprLit, File, FnArg, ImplItem, Item, Lit, Pat, Signature, Stmt};

// --------------------------------------------------------------------------- the walk

pub(crate) struct RsState<'a> {
    pub(crate) file: &'a str,
    pub(crate) source: &'a str,
    pub(super) file_name: String,
    /// Lines (and the line below) carrying a real comment — an #[allow] on
    /// one of them has its reason.
    pub(super) reason_lines: std::collections::HashSet<usize>,
    pub(crate) findings: Vec<Finding>,
    pub(super) cc: Vec<FnCc>,
    pub(super) skeletons: Vec<crate::skeleton::SkeletonFn>,
    pub(super) mod_decls: Vec<(String, bool)>,
    pub(super) uses: Vec<String>,
    pub(crate) structs: Vec<RsStruct>,
    pub(super) supps: crate::suppressions::Suppressions,
    pub(super) supps_spent: std::collections::HashSet<(usize, String)>,
    /// (self type name, method count) — record-shape post-pass.
    pub(super) impls: Vec<(String, usize)>,
    /// (fn name, line, param type idents) — the boundary record-shape check.
    pub(super) fn_params: Vec<(String, usize, Vec<String>)>,
    /// (fn name, line, first param type) of module-level free fns — the
    /// strewing post-pass (3+ free fns sharing a leading struct param).
    pub(super) strew_candidates: Vec<(String, usize, String)>,
    pub(super) fn_stack: Vec<RsFnScope>,
    pub(super) current_fn: Option<(String, usize)>,
    /// Parent chain for the magic-number position rule.
    pub(super) expr_stack: Vec<&'a Expr>,
    pub(super) in_test_code: bool,
    pub(super) is_test_file: bool,
    pub(super) repo_wide: bool,
    /// The `impl` type whose methods are being walked — the owner the
    /// unnamed-condition message names.
    pub(super) current_impl: Option<String>,
    /// The function body is exactly one expression (a `use` line does not
    /// count): a match there is the function's own definition.
    pub(super) bare_body: bool,
    /// How many of the function's `return` statements carry a non-literal
    /// value — exactly one means that return is the function's answer.
    pub(super) valued_returns: usize,
    /// The statement about to be visited is its block's tail expression. The
    /// walk sets this, so no check has to compare spans to find out where it
    /// is.
    pub(super) at_tail: bool,
    /// The block about to be visited is the current function's body (not a
    /// nested block) — where `at_tail` means something.
    pub(super) body_block: bool,
}

pub(super) struct RsFnScope {
    decisions: u32,
}

impl<'a> RsState<'a> {
    pub(crate) fn walk_file(&mut self, file: &'a File) {
        for item in &file.items {
            self.visit_item(item);
        }
    }

    pub(crate) fn finish(mut self) -> RustScan {
        let structs = std::mem::take(&mut self.structs); // the post-passes consume them
                                                         // record-shape: structs with >= 5 named fields and no methods are
                                                         // data bags; the finding lands on FUNCTIONS taking one as a param
                                                         // (the Python family's boundary orientation — a DTO definition is
                                                         // not the problem, a data bag crossing a function boundary is)
        let record_names: std::collections::HashSet<String> = structs
            .iter()
            .filter(|s| {
                s.fields >= 5
                    && self
                        .impls
                        .iter()
                        .filter(|(n, _)| *n == s.name)
                        .map(|(_, m)| m)
                        .sum::<usize>()
                        == 0
            })
            .map(|s| s.name.clone())
            .collect();
        let record_fields: HashMap<String, usize> = structs.iter().map(|s| (s.name.clone(), s.fields)).collect();
        let methods_by: HashMap<String, usize> = self.impls.iter().fold(HashMap::new(), |mut m, (n, c)| {
            *m.entry(n.clone()).or_insert(0) += c;
            m
        });
        // vague-name: a role-suffix struct that carries real weight (large
        // span or several methods) — the domain noun should take the name
        for s in &structs {
            let methods = methods_by.get(&s.name).copied().unwrap_or(0);
            if crate::naming::vague_role_is_loaded(&s.name, s.span, methods) {
                self.finding(
                    "vague-name",
                    "fail",
                    s.line,
                    "",
                    format!(
                        "'{}' name carries a {}-line struct with {methods} methods — the domain noun should take the name",
                        suffix_of(&s.name).unwrap_or(""), s.span
                    ),
                );
            }
        }
        if !record_names.is_empty() {
            let fn_params = std::mem::take(&mut self.fn_params);
            for (fn_name, line, types) in fn_params {
                if let Some(struct_name) = types.iter().find(|t| record_names.contains(*t)) {
                    let fields = record_fields.get(struct_name).copied().unwrap_or(0);
                    self.finding(
                        "record-shape",
                        "fail",
                        line,
                        &fn_name,
                        format!(
                            "function '{fn_name}' takes record-shaped struct '{struct_name}' ({fields} fields, no methods) — its rules belong as methods on the struct"
                        ),
                    );
                }
            }
        }
        // strewing: 3+ module-level free fns sharing a leading param that is
        // a struct defined in this file — a missed class (the Python rule)
        let strew = std::mem::take(&mut self.strew_candidates);
        let struct_names: std::collections::HashSet<String> = structs.iter().map(|s| s.name.clone()).collect();
        let mut by_param: std::collections::HashMap<&str, Vec<&(String, usize, String)>> =
            std::collections::HashMap::new();
        for c in &strew {
            if struct_names.contains(&c.2) {
                by_param.entry(c.2.as_str()).or_default().push(c);
            }
        }
        let mut strew_groups: Vec<(usize, usize, String)> = by_param
            .iter()
            .filter(|(_, g)| g.len() >= 3)
            .map(|(param, g)| {
                let fns = g.iter().map(|c| c.0.as_str()).collect::<Vec<_>>().join("', '");
                (g[0].1, g.len(), format!("{param}|{fns}"))
            })
            .collect();
        strew_groups.sort();
        for (line, count, detail) in strew_groups {
            let (param, fns) = detail.split_once('|').unwrap_or((detail.as_str(), ""));
            self.finding(
                "strewing",
                "fail",
                line,
                "",
                format!(
                    "{count} free functions take '{param}' as their first parameter ('{fns}'). The struct names the data, but no method owns it. Give '{param}' an impl and move these functions onto it."
                ),
            );
        }
        RustScan {
            supps: self.supps.clone(),
            supps_spent: self.supps_spent,
            file_name: self.file_name.clone(),
            findings: self.findings,
            cc: self.cc,
            errors: 0,
            skeletons: self.skeletons,
            mod_decls: self.mod_decls,
            uses: self.uses,
            module_shape: Default::default(),
        }
    }

    /// `#[allow(...)]` / `#[expect(...)]` without a reason comment on the
    /// same line or the line above is a finding (house standard: every
    /// suppression carries a why — Rust's compiler-lint exemptions included).
    fn allow_reason_check(&mut self, a: &Attribute) {
        if !(a.path().is_ident("allow") || a.path().is_ident("expect")) {
            return;
        }
        let line = a.span().start().line;
        if self.reason_lines.contains(&line) {
            return;
        }
        self.finding(
            "allow-reason",
            "fail",
            line,
            "",
            format!(
                "#[{}(...)] at line {line} without a reason — every suppression carries a why: add `// reason: ...`",
                if a.path().is_ident("allow") { "allow" } else { "expect" }
            ),
        );
    }

    fn decide(&mut self, n: u32) {
        if let Some(scope) = self.fn_stack.last_mut() {
            scope.decisions += n;
        }
    }

    pub(crate) fn finding(&mut self, kind: &str, severity: &str, line: usize, function: &str, message: String) {
        self.finding_col(kind, severity, line, 0, function, message);
    }

    /// Same, with the schema-3 anchor column (1-based; 0 = line-level).
    // lucidlint: ignore long-param-list one caller — the struct would be ceremony for a single call site
    fn finding_col(&mut self, kind: &str, severity: &str, line: usize, col: usize, function: &str, message: String) {
        // Phase 4a: the Rust layer records no logical line — its findings
        // anchor on the item or statement that starts the construct (`fn`/
        // `struct` line, the literal's line), so `logical_start: None` already
        // means the finding's own line, exactly the marker window this layer
        // has always used.
        self.findings.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col,
            file: self.file.to_string(),
            line,
            function: function.to_string(),
            kind: kind.into(),
            severity: severity.into(),
            message,
        });
    }

    /// A module-level free fn whose leading parameter's type it uses — the
    /// strewing pass's candidate. The param must be USED: "they share data" —
    /// a fn that never reads it would make the extracted class field-disjoint
    /// (partition) and the fix would thrash.
    fn collect_strew_candidate(&mut self, sig: &Signature, block: &'a Block, line: usize) {
        let Some(FnArg::Typed(pt)) = sig.inputs.first() else {
            return;
        };
        let Pat::Ident(pi) = pt.pat.as_ref() else { return };
        if !tokens_contain_ident(quote::ToTokens::to_token_stream(block), &pi.ident.to_string()) {
            return;
        }
        let mut types = Vec::new();
        collect_type_idents(&pt.ty, &mut types);
        if let Some(t) = types.first() {
            self.strew_candidates.push((sig.ident.to_string(), line, t.clone()));
        }
    }

    /// Record the function's shape — its body as one expression, and how many
    /// returns carry a value. The checks read these facts; none of them
    /// compares a node against a recorded span to work out where it is.
    fn track_returned_value(&mut self, block: &'a Block) -> (bool, usize) {
        let prev_bare = self.bare_body;
        let prev_returns = self.valued_returns;
        // the body is exactly one expression (a `use` line does not count)
        let body: Vec<&Stmt> = block
            .stmts
            .iter()
            .filter(|s| !matches!(s, Stmt::Item(Item::Use(_))))
            .collect();
        self.bare_body = matches!(body.as_slice(), [Stmt::Expr(_, None)]);
        self.valued_returns = return_values(block)
            .into_iter()
            .filter(|v| !matches!(v, Expr::Lit(_) | Expr::Path(_)))
            .count();
        (prev_bare, prev_returns)
    }

    /// The file is scanned repo-wide and is not test code — the skeleton
    /// identity's scope.
    fn is_repo_wide_module(&self, module_level: bool) -> bool {
        module_level && self.repo_wide && !self.is_test_file
    }

    /// The method never uses its receiver and is not a test — it does not
    /// belong in the impl.
    fn is_detached_method(&self, f: &syn::ImplItemFn) -> bool {
        f.sig.receiver().is_some() && !has_attr(&f.attrs, "test") && !self.in_test_code && !block_refs_self(&f.block)
    }

    /// `unnamed-condition`: three or more facts joined inline as a condition,
    /// about one or more nouns, with no name for the decision.
    fn unnamed_condition(&mut self, expr: &'a Expr, is_tail: bool) {
        let cond = match expr {
            Expr::If(i) => &i.cond,
            Expr::While(w) => &w.cond,
            _ => return,
        };
        // a tail condition is the function's answer — except a control-flow
        // tail, which returns a choice and leaves the condition inside unnamed
        if is_tail {
            return;
        }
        let operands = rs_bool_operands(cond);
        if operands.len() < 3 {
            return;
        }
        let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        if matches!(fn_name.as_str(), "eq" | "ne" | "cmp" | "partial_cmp" | "hash") {
            return; // the operator IS the decision: the name exists
        }
        let nouns: Vec<String> = rs_condition_nouns(cond)
            .into_iter()
            .filter(|n| !rs_names_a_type_or_constant(&self.structs, n))
            .collect();
        if !names_a_decision(&nouns) {
            return;
        }
        let message = rs_condition_message(operands.len(), &nouns, self.current_impl.as_deref());
        self.finding("unnamed-condition", "warn", cond.span().start().line, &fn_name, message);
    }

    /// The match is the function's answer — its whole body, or its only valued
    /// return — so the function's name already names the classification.
    fn is_function_answer(&self, is_tail: bool, returned: bool) -> bool {
        (is_tail && self.bare_body) || (returned && self.valued_returns == 1)
    }

    /// `unnamed-classification`: a match sorting one value into constants, with
    /// no name for the kinds.
    fn unnamed_classification(&mut self, expr: &'a Expr, is_tail: bool) {
        let Expr::Match(m) = expr else { return };
        let returned = matches!(self.expr_stack.last(), Some(Expr::Return(_)));
        if self.is_function_answer(is_tail, returned) {
            return;
        }
        // otherwise the classification must be the whole answer of its
        // statement: a match used as one argument or operand is an expression,
        // not a named kind
        if matches!(self.expr_stack.last(), Some(parent) if !matches!(parent, Expr::Return(_))) {
            return;
        }
        let cases: Vec<&Arm> = m.arms.iter().filter(|a| !matches!(a.pat, Pat::Wild(_))).collect();
        if cases.len() < 3 {
            return;
        }
        let classifies = cases
            .iter()
            .all(|a| matches!(a.body.as_ref(), Expr::Lit(_) | Expr::Path(_)));
        if !classifies {
            return;
        }
        let subject = rs_expr_text(&m.expr);
        let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        self.finding(
            "unnamed-classification",
            "warn",
            expr.span().start().line,
            &fn_name,
            format!(
                "{} cases of {subject} are classified here, and the classification has no name. Name the cases as one kind, and return that kind.",
                cases.len()
            ),
        );
    }

    /// Magic numbers: int/float literals outside (0, 1, 2) whose parent is an
    /// operation — the same position rule as the Python layer. Rust const
    /// definitions (const/static items, enum discriminants) are naturally
    /// exempt: their literals' parents are not operations.
    fn magic_check(&mut self, lit: &'a ExprLit) {
        let text = match &lit.lit {
            Lit::Int(i) => i.base10_digits().to_string(),
            Lit::Float(f) => f.base10_digits().to_string(),
            _ => return,
        };
        if !crate::magic::is_magic_value(&text) {
            return;
        }
        let parent_is_op = matches!(
            self.expr_stack.last(),
            Some(Expr::Binary(_) | Expr::Unary(_) | Expr::Index(_) | Expr::Call(_) | Expr::MethodCall(_))
        );
        if !parent_is_op {
            return;
        }
        let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        let line = lit.span().start().line;
        let col = lit.span().start().column + 1; // syn columns are 0-based
        self.finding_col(
            "magic-number",
            "warn",
            line,
            col,
            &fn_name,
            // no fix: directive — the Rust fix surface refuses magic-number
            // (main.rs --fix handles extract-method/dispatch-registry/rule-table
            // only); advertising it would send agents on a fix that cannot
            // apply (review bot)
            format!(
                "magic number {text} — name it with a domain noun (what it means here), never its value spelled out"
            ),
        );
    }

    /// Expression statements that discard a value — the Python noop family.
    /// Calls/macros/assignments/strings/literals/closures are harmless; a bare
    /// path, arithmetic, an if/match whose result is thrown away is a dead
    /// statement. Blocks are harmless (scoping is idiomatic Rust).
    fn noop_check(&mut self, e: &'a Expr) {
        let harmless = matches!(
            e,
            Expr::Call(_)
                | Expr::MethodCall(_)
                | Expr::Macro(_)
                | Expr::Await(_)
                | Expr::Yield(_)
                | Expr::Closure(_)
                | Expr::Lit(_)
                | Expr::Assign(_)
                | Expr::Block(_)
                | Expr::Async(_)
                | Expr::Try(_)
        );
        // syn 2 has no Expr::AssignOp — `x += 1` is Expr::Binary with a
        // compound BinOp; treat those as real statements, not dead arithmetic
        let compound_assign = matches!(e, Expr::Binary(b) if is_compound_op(&b.op));
        // return/break/continue are control flow, not discarded values
        let control = matches!(e, Expr::Return(_) | Expr::Break(_) | Expr::Continue(_));
        if is_discarded_expression(compound_assign, control, harmless) {
            let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
            let line = e.span().start().line;
            self.finding(
                "noop-statement",
                "fail",
                line,
                &fn_name,
                "expression statement discards its value — delete the dead statement".into(),
            );
        }
    }

    /// One function at any depth: push a decision scope, walk the body, then
    /// the cc/closures/large-function/skeleton bookkeeping. Test fns skip the
    /// production families.
    fn process_fn(&mut self, attrs: &[Attribute], sig: &Signature, block: &'a Block) {
        let is_test_fn = has_attr(attrs, "test");
        let line = sig.span().start().line;
        let module_level = self.fn_stack.is_empty();
        let prev_test = self.in_test_code;
        if is_test_fn {
            self.in_test_code = true;
        }
        let prev_fn = self.current_fn.take();
        self.current_fn = Some((sig.ident.to_string(), line));
        let (prev_bare, prev_returns) = self.track_returned_value(block);
        self.fn_stack.push(RsFnScope { decisions: 0 });
        self.body_block = true; // this block's tail is the function's answer
        self.visit_block(block); // the override: unreachable check + nested walk
                                 // the push/pop invariant is per-function; a missing scope is a walker
                                 // bug, not a crash — default to no decisions
        let scope = self.fn_stack.pop().unwrap_or(RsFnScope { decisions: 0 });
        self.bare_body = prev_bare;
        self.valued_returns = prev_returns;
        let cc = scope.decisions + 1;
        let span = span_lines(sig.span().start().line, block.span().end().line);
        if !self.in_test_code {
            // closures->latent-class: >= 2 inner fns/closures on a big fn
            let inner = inner_count(block);
            if is_closures_shape(inner, cc, span) {
                self.finding(
                    "closures",
                    "fail",
                    line,
                    &sig.ident.to_string(),
                    format!(
                        "'{}' defines {inner} nested fn definitions closing over its state — a class in disguise",
                        sig.ident
                    ),
                );
            }
            // the loop family (collection-build / fold / sequential-pass
            // verdicts) — per-function shapes, own module below
            crate::rustloops::rust_loop_findings(self, sig, block, &sig.ident.to_string(), line);
            let typed = sig.inputs.iter().filter(|i| matches!(i, FnArg::Typed(_))).count();
            if typed > 5 && !is_trivial_rust_stub(&block.stmts) {
                self.finding(
                    "long-param-list",
                    "fail",
                    line,
                    &sig.ident.to_string(),
                    format!(
                        "'{}' takes {typed} parameters — introduce a parameter object named with a domain noun",
                        sig.ident
                    ),
                );
            }
            if span >= 120 {
                self.finding(
                    "large-function",
                    "fail",
                    line,
                    &sig.ident.to_string(),
                    format!(
                        "function '{}' spans {span} lines (>= 120) — Extract Function: split it into one rule per named method",
                        sig.ident
                    ),
                );
            }
            if self.is_repo_wide_module(module_level) {
                let skel = rs_skeleton(sig, block);
                if crate::skeleton::is_duplicate_size(skel.len(), block.stmts.len()) {
                    self.skeletons.push(crate::skeleton::SkeletonFn {
                        rel: self.file.to_string(),
                        name: sig.ident.to_string(),
                        line,
                        end_line: block.span().end().line,
                        skeleton: skel,
                    });
                }
            }
        }
        if module_level && !self.in_test_code {
            // the gate reads cc from the scan's own array — module-level fns
            // only, mirroring radon's fn_map (methods/nested get cc = 0)
            let (shape, shape_detail) = rust_fn_shape(&block.stmts, sig);
            self.cc.push(FnCc {
                file: self.file.to_string(),
                function: sig.ident.to_string(),
                line,
                cc,
                shape,
                shape_detail,
            });
        }
        if !self.in_test_code {
            let mut types = Vec::new();
            for input in &sig.inputs {
                if let FnArg::Typed(pt) = input {
                    collect_type_idents(&pt.ty, &mut types);
                }
            }
            types.sort();
            types.dedup();
            self.fn_params.push((sig.ident.to_string(), line, types));
            if module_level {
                self.collect_strew_candidate(sig, block, line);
            }
        }
        self.current_fn = prev_fn;
        self.in_test_code = prev_test;
    }
}

impl<'a> Visit<'a> for RsState<'a> {
    fn visit_item(&mut self, item: &'a Item) {
        if has_cfg_test(attrs_of(item)) {
            return; // #[cfg(test)] subtrees are test code — skipped
        }
        // every #[allow]/#[expect] needs a reason comment on its line or the
        // line above (the house standard; cfg(test) subtrees are skipped)
        for a in attrs_of(item) {
            self.allow_reason_check(a);
        }
        match item {
            Item::Fn(f) => self.process_fn(&f.attrs, &f.sig, &f.block),
            Item::Mod(m) => {
                self.mod_decls.push((m.ident.to_string(), m.content.is_some()));
                if let Some((_, items)) = &m.content {
                    for it in items {
                        self.visit_item(it);
                    }
                }
            }
            Item::Use(u) => {
                self.uses.extend(use_paths(u));
            }
            Item::Struct(s) => {
                if let syn::Fields::Named(named) = &s.fields {
                    self.structs.push(RsStruct {
                        name: s.ident.to_string(),
                        line: s.span().start().line,
                        span: span_lines(s.span().start().line, s.span().end().line),
                        fields: named.named.len(),
                    });
                }
            }
            Item::Static(s) => {
                let ty_ident = static_type_ident(&s.ty);
                let interior = [
                    "Mutex",
                    "RwLock",
                    "RefCell",
                    "UnsafeCell",
                    "AtomicBool",
                    "AtomicPtr",
                    "AtomicUsize",
                    "AtomicIsize",
                    "AtomicU8",
                    "AtomicU16",
                    "AtomicU32",
                    "AtomicU64",
                    "AtomicI8",
                    "AtomicI16",
                    "AtomicI32",
                    "AtomicI64",
                ];
                if matches!(s.mutability, syn::StaticMutability::Mut(_)) || interior.contains(&ty_ident.as_str()) {
                    self.finding(
                        "global-state",
                        "fail",
                        s.span().start().line,
                        "",
                        "static mutable state — put it in a struct owned by the caller".into(),
                    );
                }
                visit::visit_item(self, item);
            }
            Item::Impl(i) => {
                let self_name = impl_self_name(i);
                let methods = i.items.iter().filter(|it| matches!(it, ImplItem::Fn(_))).count();
                if let Some(n) = &self_name {
                    self.impls.push((n.clone(), methods));
                }
                let prev_impl = self.current_impl.take();
                self.current_impl = self_name;
                for it in &i.items {
                    let iattrs: &[Attribute] = match it {
                        ImplItem::Fn(f) => f.attrs.as_slice(),
                        ImplItem::Const(c) => c.attrs.as_slice(),
                        ImplItem::Type(t) => t.attrs.as_slice(),
                        ImplItem::Macro(m) => m.attrs.as_slice(),
                        _ => &[],
                    };
                    for a in iattrs {
                        self.allow_reason_check(a);
                    }
                    if let ImplItem::Fn(f) = it {
                        // impl methods: same checks, but never module-level
                        let saved = std::mem::take(&mut self.fn_stack);
                        self.fn_stack.push(RsFnScope { decisions: 0 });
                        // reuse process_fn via a nested item: the method body
                        // needs the impl's fn shape
                        self.process_fn(&f.attrs, &f.sig, &f.block);
                        self.fn_stack = saved;
                        // detached-method: a receiver that the body never uses
                        // — the method does not touch instance state, so it
                        // does not belong in the impl (the Python rule's
                        // inverse direction, Rust-flavored)
                        if self.is_detached_method(f) {
                            let line = f.sig.span().start().line;
                            self.finding(
                                "detached-method",
                                "warn",
                                line,
                                &f.sig.ident.to_string(),
                                format!(
                                    "method '{}' never uses its receiver — it does not touch instance state; make it an associated fn or move it out of the impl",
                                    f.sig.ident
                                ),
                            );
                        }
                    }
                }
                self.current_impl = prev_impl;
            }
            _ => visit::visit_item(self, item), // enums, traits, consts, statics, types
        }
    }

    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        if let Stmt::Local(l) = stmt {
            for a in &l.attrs {
                self.allow_reason_check(a);
            }
        }
        if !self.in_test_code {
            // swallow: `let _ = <value>;` discards a call/macro result — the
            // catch-that-vanishes analog (a Result/Option dropped on the
            // floor). Plain-path inits are excluded: moving a value into `_`
            // is sometimes the only way to bind it.
            if let Stmt::Local(l) = stmt {
                if matches!(l.pat, Pat::Wild(_)) {
                    if let Some(init) = &l.init {
                        if matches!(
                            init.expr.as_ref(),
                            Expr::Call(_) | Expr::MethodCall(_) | Expr::Macro(_) | Expr::Await(_)
                        ) {
                            let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
                            self.finding(
                                "swallow",
                                "fail",
                                stmt.span().start().line,
                                &fn_name,
                                "`let _ =` discards a value — if it's a Result/Option the error vanishes; handle it (match / ? / if let)"
                                    .into(),
                            );
                        }
                    }
                }
            }
            if let Stmt::Expr(e, Some(_)) = stmt {
                self.noop_check(e);
            }
            // statement-position macros (dbg!(x);) parse as Stmt::Macro and
            // never reach the Expr::Macro arm in visit_expr — the common
            // form must be checked here too
            if let Stmt::Macro(m) = stmt {
                if is_dbg_macro(&m.mac.path) {
                    let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
                    self.finding(
                        "debug-artifact",
                        "fail",
                        stmt.span().start().line,
                        &fn_name,
                        "dbg!() left in production code — remove it (clippy's dbg_macro lint is pedantic-only)".into(),
                    );
                }
            }
        }
        visit::visit_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        // the tail flag belongs to the statement, not to expressions inside it
        let is_tail = self.at_tail && self.expr_stack.is_empty();
        if is_tail {
            self.at_tail = false;
        }
        if !self.in_test_code {
            match expr {
                Expr::Lit(l) => self.magic_check(l),
                Expr::If(_) => {
                    self.decide(1);
                    self.unnamed_condition(expr, is_tail);
                }
                Expr::Match(m) => {
                    let arms = m.arms.iter().filter(|a| !matches!(a.pat, Pat::Wild(_))).count() as u32;
                    self.decide(arms);
                    self.unnamed_classification(expr, is_tail);
                }
                Expr::Binary(b) => {
                    if matches!(b.op, BinOp::And(_) | BinOp::Or(_)) {
                        self.decide(1);
                    }
                }
                Expr::While(_) => {
                    self.decide(1);
                    self.unnamed_condition(expr, is_tail);
                }
                Expr::ForLoop(_) | Expr::Loop(_) => self.decide(1),
                Expr::Macro(m) if is_assert_macro(&m.mac.path) => {
                    self.decide(1); // assert!: +1, subtree not counted (visit_Assert)
                }
                Expr::Macro(m) if is_dbg_macro(&m.mac.path) => {
                    let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
                    self.finding(
                        "debug-artifact",
                        "fail",
                        expr.span().start().line,
                        &fn_name,
                        "dbg!() left in production code — remove it (clippy's dbg_macro lint is pedantic-only)".into(),
                    );
                }
                Expr::MethodCall(mc) if is_unwrap_expect(&mc.method) => {
                    let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
                    self.finding(
                        "debug-artifact",
                        "fail",
                        expr.span().start().line,
                        &fn_name,
                        format!(
                            ".{}() in production code panics on None/Err — handle the case or return a Result",
                            mc.method
                        ),
                    );
                }
                Expr::Call(c) => {
                    for arg in &c.args {
                        if matches!(arg, Expr::Lit(l) if matches!(l.lit, Lit::Bool(_))) {
                            let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
                            self.finding(
                                "boolean-arg",
                                "fail",
                                arg.span().start().line,
                                &fn_name,
                                "boolean literal argument — name it: f(..., retry=True)".into(),
                            );
                        }
                    }
                }
                _ => {}
            }
        } else {
            // test-only family: fakefs — fs I/O in a test with a literal
            // path that is not a temp dir (tests fake the filesystem).
            // Two shapes: `File::create("x")` / `fs::write("x", ..)` parse as
            // Expr::Call on a Path callee; `x.write_all(b"..")` is a method
            // call on a value (skipped — the receiver is not fs).
            if fakefs_hit(expr) {
                let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
                self.finding(
                    "fakefs",
                    "fail",
                    expr.span().start().line,
                    &fn_name,
                    "real filesystem I/O in a test — write to a temp dir (std::env::temp_dir, tempfile), not a literal path"
                        .into(),
                );
            }
        }
        self.expr_stack.push(expr);
        visit::visit_expr(self, expr);
        self.expr_stack.pop();
    }

    fn visit_block(&mut self, block: &'a Block) {
        if !self.in_test_code {
            // statements after an unconditional return/break/continue or a
            // diverging macro are dead — one finding per statement list
            let mut dead = false;
            for stmt in &block.stmts {
                if dead {
                    let line = stmt.span().start().line;
                    let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
                    self.finding(
                        "unreachable",
                        "fail",
                        line,
                        &fn_name,
                        format!("unreachable statement at line {line} — dead code is deleted"),
                    );
                    break;
                }
                if is_diverge_stmt(stmt) {
                    dead = true;
                }
            }
        }
        let is_body = self.body_block;
        self.body_block = false;
        let last = block.stmts.len().saturating_sub(1);
        for (index, stmt) in block.stmts.iter().enumerate() {
            let prev = self.at_tail;
            // only the FUNCTION body's last expression is its answer: a nested
            // block's tail is just where that block ends
            self.at_tail = is_body && index == last && matches!(stmt, Stmt::Expr(_, None));
            self.visit_stmt(stmt);
            self.at_tail = prev;
        }
    }
}

// --------------------------------------------------------------------------- helpers

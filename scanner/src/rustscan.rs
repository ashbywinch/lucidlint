// code-health: ignore-file complexity the syn walkers are single dispatch tables — match-arm count,
// not branching; keep NEW functions under cc 15

//! The Rust language layer — syn-based per-file scan.
//!
//! Seam: the same `Finding`/`FnCc` model as the Python layer, the same shared
//! logic from `common` (duplicate similarity, suppression matching, the CC
//! rule table, vague-role names) — but the AST walk is genuinely Rust-shaped
//! (`syn`), and the families whose Rust analog is weak or compiler-owned
//! (monkeypatch fixtures — Rust injects via traits; class-module naming —
//! cargo owns module structure; builtin shadowing — clippy's restriction
//! set) do not exist here while Rust-only concerns (an `#[allow]` without a
//! reason, `#[ignore]`d tests) do. `swallow` here is `let _ = <value>;` —
//! the catch-that-vanishes analog; `strewing` is free fns sharing a leading
//! struct param. `use`/`mod`
//! collection feeds the cross-file import-cycle family; the code-review-graph
//! families (hub-file, high-risk, layer-mix, hotspot) degrade for Rust repos
//! until the exporter speaks Rust, and `unused` is deliberately NOT re-
//! implemented — rustc's dead_code owns that with better precision.
//!
//! Test code: `#[cfg(test)]` items and `#[test]` fn bodies are skipped by the
//! production families (their literals/decisions are noise, and Rust keeps
//! tests inline where Python files them separately) — except the rules that
//! LIVE in tests: an `#[ignore]`d test is a skipif finding, and suppressions
//! still parse from the whole file.

use crate::{common, Finding, FnCc};
use std::collections::HashMap;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Attribute, BinOp, Block, Expr, ExprCall, ExprLit, ExprMacro, File, FnArg, Ident, ImplItem, Item, ItemFn, ItemImpl,
    ItemUse, Lit, Pat, Path, Signature, Stmt, Type, UseTree,
};

/// One struct: name, declaration line, named-field count, line span.
#[derive(Clone)]
pub struct RsStruct {
    pub name: String,
    pub line: usize,
    pub fields: usize,
    pub span: usize,
}

/// The Rust scan output for one file — the FileScan-shaped parts plus the
/// module-graph data the cross-file families need.
pub struct RustScan {
    pub file_name: String,
    pub findings: Vec<Finding>,
    pub cc: Vec<FnCc>,
    pub errors: usize,
    pub skeletons: Vec<common::SkeletonFn>,
    /// (module name, inline?) — top-level `mod` declarations.
    pub mod_decls: Vec<(String, bool)>,
    /// Raw `use` paths ("crate::a::b", "super::x", "serde::Deserialize", ...).
    pub uses: Vec<String>,
}

impl RustScan {
    pub fn file_name(&self) -> String {
        self.file_name.clone()
    }
}

pub fn scan_source(source: &str, name: &str, repo_wide: bool) -> RustScan {
    let file = match syn::parse_file(source) {
        Ok(f) => f,
        Err(_) => {
            return RustScan {
                file_name: name.to_string(),
                findings: Vec::new(),
                cc: Vec::new(),
                errors: 1,
                skeletons: Vec::new(),
                mod_decls: Vec::new(),
                uses: Vec::new(),
            };
        }
    };
    let reason_lines: std::collections::HashSet<usize> = rs_comment_lines(source)
        .into_iter()
        .filter(|(_, t)| !t.trim_start_matches(['/', '*', ' ']).trim().is_empty())
        .flat_map(|(l, _)| [l, l + 1])
        .collect();

    let mut state = RsState {
        file: name,
        file_name: name.to_string(),
        reason_lines,
        findings: Vec::new(),
        cc: Vec::new(),
        skeletons: Vec::new(),
        mod_decls: Vec::new(),
        uses: Vec::new(),
        structs: Vec::new(),
        impls: Vec::new(),
        fn_params: Vec::new(),
        strew_candidates: Vec::new(),
        fn_stack: Vec::new(),
        current_fn: None,
        expr_stack: Vec::new(),
        in_test_code: false,
        is_test_file: crate::is_test_path(name),
        repo_wide,
    };
    // the test-only rules run even inside #[cfg(test)] mods — a pre-pass over
    // the whole file, then the production walk skips test subtrees
    state.findings.extend(ignored_test_findings(&file, name));
    state.findings.extend(no_assert_test_findings(&file, name));
    state.walk_file(&file);
    let mut scan = state.finish();
    // suppressions parse from the whole file, filtering every family — the
    // shared why-less rule applies (`// code-health: ignore` needs a why)
    let comments = rs_comment_lines(source);
    let supps = common::suppressions_from_comments(&comments);
    // complexity findings are generated from the cc array AFTER the per-file
    // pass — honor complexity suppressions here so both paths agree, and
    // record which suppressions this retain honored so the stale check does
    // not re-flag them
    let mut pre_used = common::PreUsedSuppressions::default();
    if !scan.cc.is_empty() {
        scan.cc.retain(|e| {
            for ln in [e.line, e.line.saturating_sub(1)] {
                if let Some(entries) = supps.line.get(&ln) {
                    for (sig, why) in entries {
                        if sig == "complexity" && !why.is_empty() {
                            pre_used.lines.insert((ln, sig.clone()));
                        }
                    }
                }
            }
            let line_suppressed = common::suppressed("complexity", e.line, &supps);
            let file_suppressed = supps.file.get("complexity").is_some_and(|w| !w.is_empty());
            if file_suppressed {
                pre_used.files.insert("complexity".into());
            }
            !line_suppressed && !file_suppressed
        });
    }
    scan.findings = common::apply_suppressions_impl(scan.findings, &comments, name, "//", &pre_used);
    scan
}

// --------------------------------------------------------------------------- the walk

struct RsState<'a> {
    file: &'a str,
    file_name: String,
    /// Lines (and the line below) carrying a real comment — an #[allow] on
    /// one of them has its reason.
    reason_lines: std::collections::HashSet<usize>,
    findings: Vec<Finding>,
    cc: Vec<FnCc>,
    skeletons: Vec<common::SkeletonFn>,
    mod_decls: Vec<(String, bool)>,
    uses: Vec<String>,
    structs: Vec<RsStruct>,
    /// (self type name, method count) — record-shape post-pass.
    impls: Vec<(String, usize)>,
    /// (fn name, line, param type idents) — the boundary record-shape check.
    fn_params: Vec<(String, usize, Vec<String>)>,
    /// (fn name, line, first param type) of module-level free fns — the
    /// strewing post-pass (3+ free fns sharing a leading struct param).
    strew_candidates: Vec<(String, usize, String)>,
    fn_stack: Vec<RsFnScope>,
    current_fn: Option<(String, usize)>,
    /// Parent chain for the magic-number position rule.
    expr_stack: Vec<&'a Expr>,
    in_test_code: bool,
    is_test_file: bool,
    repo_wide: bool,
}

struct RsFnScope {
    decisions: u32,
}

impl<'a> RsState<'a> {
    fn walk_file(&mut self, file: &'a File) {
        for item in &file.items {
            self.visit_item(item);
        }
    }

    fn finish(mut self) -> RustScan {
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
            if common::vague_role_is_loaded(&s.name, s.span, methods) {
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
                (g[0].1, g.len(), format!("'{}' shared by {fns}", param))
            })
            .collect();
        strew_groups.sort();
        for (line, count, detail) in strew_groups {
            self.finding(
                "strewing",
                "fail",
                line,
                "",
                format!("{count} free functions take the same leading parameter ({detail}) — they share data, extract a class"),
            );
        }
        RustScan {
            file_name: self.file_name.clone(),
            findings: self.findings,
            cc: self.cc,
            errors: 0,
            skeletons: self.skeletons,
            mod_decls: self.mod_decls,
            uses: self.uses,
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

    fn finding(&mut self, kind: &str, severity: &str, line: usize, function: &str, message: String) {
        self.findings.push(Finding {
            file: self.file.to_string(),
            line,
            function: function.to_string(),
            kind: kind.into(),
            severity: severity.into(),
            message,
        });
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
        if !common::is_magic_value(&text) {
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
        self.finding(
            "magic-number",
            "warn",
            line,
            &fn_name,
            format!("magic number {text} — name it as a constant"),
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
        if !compound_assign && !control && !harmless {
            let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
            let line = e.span().start().line;
            self.finding(
                "noop-statement",
                "fail",
                line,
                &fn_name,
                "expression statement discards its value — dead statement".into(),
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
        self.fn_stack.push(RsFnScope { decisions: 0 });
        self.visit_block(block); // the override: unreachable check + nested walk
                                 // the push/pop invariant is per-function; a missing scope is a walker
                                 // bug, not a crash — default to no decisions
        let scope = self.fn_stack.pop().unwrap_or(RsFnScope { decisions: 0 });
        let cc = scope.decisions + 1;
        let span = span_lines(sig.span().start().line, block.span().end().line);
        if !self.in_test_code {
            // closures->latent-class: >= 2 inner fns/closures on a big fn
            let inner = inner_count(block);
            if inner >= 2 && (cc >= 15 || span >= 60) {
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
            let typed = sig.inputs.iter().filter(|i| matches!(i, FnArg::Typed(_))).count();
            if typed > 5 {
                self.finding(
                    "long-param-list",
                    "fail",
                    line,
                    &sig.ident.to_string(),
                    format!(
                        "'{}' takes {typed} parameters — introduce a parameter object",
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
                        "function '{}' spans {span} lines (>= 120) — split it: one rule per function",
                        sig.ident
                    ),
                );
            }
            if module_level && self.repo_wide && !self.is_test_file {
                let skel = rs_skeleton(sig, block);
                if common::is_duplicate_size(skel.len(), block.stmts.len()) {
                    self.skeletons.push(common::SkeletonFn {
                        rel: self.file.to_string(),
                        name: sig.ident.to_string(),
                        line,
                        skeleton: skel,
                    });
                }
            }
        }
        if module_level && !self.in_test_code {
            // the gate reads cc from the scan's own array — module-level fns
            // only, mirroring radon's fn_map (methods/nested get cc = 0)
            self.cc.push(FnCc {
                file: self.file.to_string(),
                function: sig.ident.to_string(),
                line,
                cc,
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
            // strewing: module-level free fns (methods have a non-empty
            // fn_stack; test fns already set in_test_code) — the leading
            // param's bare type name, filtered against file-local structs
            // in finish()
            if module_level {
                if let Some(FnArg::Typed(pt)) = sig.inputs.first() {
                    let mut tids = Vec::new();
                    collect_type_idents(&pt.ty, &mut tids);
                    if let Some(t) = tids.first() {
                        self.strew_candidates.push((sig.ident.to_string(), line, t.clone()));
                    }
                }
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
                if let Some(n) = self_name {
                    self.impls.push((n, methods));
                }
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
                        if f.sig.receiver().is_some()
                            && !has_attr(&f.attrs, "test")
                            && !self.in_test_code
                            && !block_refs_self(&f.block)
                        {
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
        }
        visit::visit_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        if !self.in_test_code {
            match expr {
                Expr::Lit(l) => self.magic_check(l),
                Expr::If(_) => self.decide(1),
                Expr::Match(m) => {
                    let arms = m.arms.iter().filter(|a| !matches!(a.pat, Pat::Wild(_))).count() as u32;
                    self.decide(arms);
                }
                Expr::Binary(b) => {
                    if matches!(b.op, BinOp::And(_) | BinOp::Or(_)) {
                        self.decide(1);
                    }
                }
                Expr::ForLoop(_) | Expr::While(_) | Expr::Loop(_) => self.decide(1),
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
        visit::visit_block(self, block);
    }
}

// --------------------------------------------------------------------------- helpers

fn attrs_of(item: &Item) -> &[Attribute] {
    match item {
        Item::Fn(f) => &f.attrs,
        Item::Mod(m) => &m.attrs,
        Item::Use(u) => &u.attrs,
        Item::Struct(s) => &s.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Enum(e) => &e.attrs,
        Item::Trait(t) => &t.attrs,
        Item::Const(c) => &c.attrs,
        Item::Static(s) => &s.attrs,
        Item::Type(t) => &t.attrs,
        Item::TraitAlias(t) => &t.attrs,
        Item::Macro(m) => &m.attrs,
        Item::Union(u) => &u.attrs,
        Item::ExternCrate(e) => &e.attrs,
        Item::ForeignMod(f) => &f.attrs,
        Item::Verbatim(_) => &[],
        _ => &[],
    }
}

fn has_attr(attrs: &[Attribute], ident: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(ident))
}

fn has_cfg_test(attrs: &[Attribute]) -> bool {
    // Check if a cfg predicate includes the `test` config option — the
    // substring approach fires on #[cfg(not(test))] and #[cfg(feature = "testing")]
    // which are NOT test-code. The correct check: the token string, split
    // by non-alphanumeric/non-underscore characters, contains the word "test"
    // AND is not disabled by a surrounding `not(test)`.
    attrs.iter().any(|a| {
        a.path().is_ident("cfg")
            && a.meta.require_list().is_ok_and(|m| {
                let token_str = m.tokens.to_string();
                let contains_not_test = token_str.contains("not(test");
                token_str
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|w| w == "test")
                    && !contains_not_test
            })
    })
}

fn is_diverge_stmt(s: &Stmt) -> bool {
    match s {
        Stmt::Expr(e, _) => {
            matches!(e, Expr::Return(_) | Expr::Break(_) | Expr::Continue(_))
                || matches!(e, Expr::Macro(m) if is_diverging_macro(&m.mac.path))
        }
        Stmt::Macro(m) => is_diverging_macro(&m.mac.path),
        _ => false,
    }
}

fn is_diverging_macro(path: &Path) -> bool {
    path.segments.last().is_some_and(|s| {
        matches!(
            s.ident.to_string().as_str(),
            "panic" | "unreachable" | "todo" | "unimplemented"
        )
    })
}

/// The matching vague role suffix of a name (for the finding message).
fn suffix_of(name: &str) -> Option<&'static str> {
    common::VAGUE_SUFFIXES.iter().find(|s| name.ends_with(**s)).copied()
}

fn is_compound_op(op: &BinOp) -> bool {
    use syn::BinOp::*;
    matches!(
        op,
        AddAssign(_)
            | SubAssign(_)
            | MulAssign(_)
            | DivAssign(_)
            | RemAssign(_)
            | BitXorAssign(_)
            | BitAndAssign(_)
            | BitOrAssign(_)
            | ShlAssign(_)
            | ShrAssign(_)
    )
}

fn is_dbg_macro(path: &Path) -> bool {
    path.segments.last().is_some_and(|s| s.ident == "dbg")
}

fn is_unwrap_expect(method: &Ident) -> bool {
    method == "unwrap" || method == "expect"
}

/// The bare type ident of a static's type (peels reference/paren wrappers).
fn static_type_ident(ty: &Type) -> String {
    match ty {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default(),
        Type::Reference(r) => static_type_ident(&r.elem),
        Type::Paren(p) => static_type_ident(&p.elem),
        _ => String::new(),
    }
}

/// std::fs::File / fs::* operations — the fake-filesystem test family.
const FS_TEST_OPS: [&str; 14] = [
    "create",
    "open",
    "write",
    "append",
    "read",
    "read_to_string",
    "read_dir",
    "remove_file",
    "remove_dir",
    "remove_dir_all",
    "rename",
    "copy",
    "create_dir",
    "create_dir_all",
];

/// The op name + container of a path-call callee: (std::fs::write ->
/// ("write", "fs")), (File::create -> ("create", "File")). Empty when the
/// callee is not an fs-path call.
fn fs_path_call(c: &ExprCall) -> Option<(String, String)> {
    let Expr::Path(p) = c.func.as_ref() else {
        return None;
    };
    let segs: Vec<syn::Ident> = p.path.segments.iter().map(|s| s.ident.clone()).collect();
    let op = segs.last().map(|i| i.to_string()).unwrap_or_default();
    if !FS_TEST_OPS.contains(&op.as_str()) {
        return None;
    }
    let container = segs
        .get(segs.len().saturating_sub(2))
        .map(|i| i.to_string())
        .unwrap_or_default();
    if container == "fs" || container == "File" {
        Some((op, container))
    } else {
        None
    }
}

/// Any string-literal argument that is a relative non-temp path.
fn lit_path_arg(args: &syn::punctuated::Punctuated<Expr, syn::token::Comma>) -> bool {
    for arg in args {
        if let Expr::Lit(l) = arg {
            if let Lit::Str(s) = &l.lit {
                let p = s.value();
                if !p.starts_with('/') && !p.to_lowercase().contains("temp") {
                    return true;
                }
            }
        }
    }
    false
}

/// Does this expression touch the real filesystem with a literal path?
/// (test-code branch only)
fn fakefs_hit(expr: &Expr) -> bool {
    match expr {
        Expr::MethodCall(mc) => {
            let op = mc.method.to_string();
            if !FS_TEST_OPS.contains(&op.as_str()) {
                return false;
            }
            matches!(
                &*mc.receiver,
                Expr::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "File" || s.ident == "fs")
            ) && lit_path_arg(&mc.args)
        }
        Expr::Call(c) => fs_path_call(c).is_some() && lit_path_arg(&c.args),
        _ => false,
    }
}

fn is_assert_macro(path: &Path) -> bool {
    path.segments.last().is_some_and(|s| {
        matches!(
            s.ident.to_string().as_str(),
            "assert" | "assert_eq" | "assert_ne" | "debug_assert" | "debug_assert_eq" | "debug_assert_ne"
        )
    })
}

/// The latent-class counter: NESTED FN DEFINITIONS at any depth.
///
/// Deliberately NOT closures: Python's rule counts lambdas because a Python
/// lambda is a discouraged single-expression shim — a smell. A Rust closure
/// is the idiomatic local abstraction (iterators, builders, callbacks) and
/// counting it would fire on every well-structured function. A nested `fn`
/// definition, by contrast, is named structure hiding inside another
/// function in BOTH languages — that is the class-in-disguise signal.
fn inner_count(block: &Block) -> u32 {
    struct Counter {
        count: u32,
    }
    impl<'ast> Visit<'ast> for Counter {
        fn visit_item_fn(&mut self, f: &'ast ItemFn) {
            self.count += 1;
            visit::visit_item_fn(self, f);
        }
    }
    let mut c = Counter { count: 0 };
    c.visit_block(block);
    c.count
}

/// The type idents referenced by a parameter type — the last segment of a
/// path plus any generic-argument paths (Vec<X>, Option<X>, &X).
fn collect_type_idents(ty: &Type, out: &mut Vec<String>) {
    match ty {
        Type::Path(tp) => {
            if let Some(seg) = tp.path.segments.last() {
                out.push(seg.ident.to_string());
                if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                    for a in &args.args {
                        if let syn::GenericArgument::Type(t) = a {
                            collect_type_idents(t, out);
                        }
                    }
                }
            }
        }
        Type::Reference(r) => collect_type_idents(&r.elem, out),
        Type::Slice(s) => collect_type_idents(&s.elem, out),
        Type::Paren(p) => collect_type_idents(&p.elem, out),
        Type::Tuple(t) => {
            for e in &t.elems {
                collect_type_idents(e, out);
            }
        }
        _ => {}
    }
}

fn impl_self_name(i: &ItemImpl) -> Option<String> {
    if let Type::Path(tp) = i.self_ty.as_ref() {
        tp.path.segments.last().map(|s| s.ident.to_string())
    } else {
        None
    }
}

fn span_lines(start: usize, end: usize) -> usize {
    end.saturating_sub(start)
}

/// `use` paths referenced by one item, raw ("crate::a::b", "super::x", ...).
/// The last segment of each path is the imported NAME — the module edge is
/// the path minus that segment, resolved cross-file.
fn use_paths(u: &ItemUse) -> Vec<String> {
    fn walk(tree: &UseTree, prefix: &mut Vec<String>, out: &mut Vec<String>) {
        match tree {
            UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                walk(&p.tree, prefix, out);
                prefix.pop();
            }
            UseTree::Name(n) => {
                prefix.push(n.ident.to_string());
                out.push(prefix.join("::"));
                prefix.pop();
            }
            UseTree::Rename(r) => {
                prefix.push(r.ident.to_string());
                out.push(prefix.join("::"));
                prefix.pop();
            }
            UseTree::Glob(_) => {
                out.push(prefix.join("::"));
            }
            UseTree::Group(g) => {
                for item in &g.items {
                    walk(item, prefix, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut prefix: Vec<String> = Vec::new();
    if u.leading_colon.is_some() {
        prefix.push(String::new()); // ::a::b — absolute
    }
    walk(&u.tree, &mut prefix, &mut out);
    out
}

// --------------------------------------------------------------------------- test-only rules

/// `#[test]` + `#[ignore]` — a skipped test rots, the skipif analog. Runs as a
/// pre-pass so cfg(test) containment can't hide it.
/// `#[test]` fn with no assertion anywhere in its body — a test that can
/// never fail. Pre-pass (cfg(test) containment can't hide it); `#[ignore]`d
/// tests are skipped (they are already skipif findings) and
/// `#[should_panic]` is an assertion contract.
fn no_assert_test_findings(file: &File, file_name: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for item in &file.items {
        walk_test_fns(item, &mut out, file_name);
    }
    out
}

fn walk_test_fns(item: &Item, out: &mut Vec<Finding>, file_name: &str) {
    match item {
        Item::Fn(f) => {
            if has_attr(&f.attrs, "test")
                && !has_attr(&f.attrs, "ignore")
                && !has_attr(&f.attrs, "should_panic")
                && !block_asserts(&f.block)
            {
                out.push(Finding {
                    file: file_name.to_string(),
                    line: f.sig.span().start().line,
                    function: f.sig.ident.to_string(),
                    kind: "no-assert-test".into(),
                    severity: "fail".into(),
                    message: "test has no assertion — it can never fail".into(),
                });
            }
        }
        Item::Mod(m) => {
            if let Some((_, items)) = &m.content {
                for it in items {
                    walk_test_fns(it, out, file_name);
                }
            }
        }
        _ => {}
    }
}

/// Any assert!/assert_eq!/.../panic! macro in the block (nested included).
fn is_assertion_macro(path: &Path) -> bool {
    let last = path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
    matches!(
        last.as_str(),
        "assert"
            | "assert_eq"
            | "assert_ne"
            | "assert_matches"
            | "debug_assert"
            | "debug_assert_eq"
            | "panic"
            | "unreachable"
            | "todo"
            | "unimplemented"
    )
}

fn block_asserts(block: &Block) -> bool {
    struct AssertProbe {
        found: bool,
    }
    impl<'a> syn::visit::Visit<'a> for AssertProbe {
        // statement-position macros (assert_eq!(..);) parse as Stmt::Macro
        // and never reach visit_expr_macro — both hooks must be covered
        fn visit_stmt_macro(&mut self, m: &'a syn::StmtMacro) {
            if is_assertion_macro(&m.mac.path) {
                self.found = true;
            }
        }
        fn visit_expr_macro(&mut self, m: &'a ExprMacro) {
            if is_assertion_macro(&m.mac.path) {
                self.found = true;
            }
            syn::visit::visit_expr_macro(self, m);
        }
    }
    let mut probe = AssertProbe { found: false };
    syn::visit::visit_block(&mut probe, block);
    probe.found
}

/// Does the block reference `self` anywhere (nested included)?
///
/// Token-based, not AST-based: `self` inside a macro invocation (`write!`,
/// `format!`) is invisible to the syn visitor — a Display::fmt that only
/// touches self via `write!` would look detached. The token stream sees it.
fn block_refs_self(block: &Block) -> bool {
    use quote::ToTokens;
    tokens_contain_self(block.to_token_stream())
}

/// Recursive: a macro invocation's tokens live in a nested group (`write!`,
/// `format!`) — only a full descent sees `self` inside them.
fn tokens_contain_self(stream: proc_macro2::TokenStream) -> bool {
    use proc_macro2::TokenTree;
    stream.into_iter().any(|tt| match tt {
        TokenTree::Ident(id) => id == "self",
        TokenTree::Group(g) => tokens_contain_self(g.stream()),
        _ => false,
    })
}

fn ignored_test_findings(file: &File, file_name: &str) -> Vec<Finding> {
    struct Collector {
        findings: Vec<Finding>,
        file: String,
    }
    impl<'ast> Visit<'ast> for Collector {
        fn visit_item_fn(&mut self, f: &'ast ItemFn) {
            if has_attr(&f.attrs, "test") && has_attr(&f.attrs, "ignore") {
                self.findings.push(Finding {
                    file: self.file.clone(),
                    line: f.sig.span().start().line,
                    function: f.sig.ident.to_string(),
                    kind: "skipif".into(),
                    severity: "fail".into(),
                    message: format!(
                        "#[ignore]d test '{}' — skipped tests rot; fix the test or delete it, never park it",
                        f.sig.ident
                    ),
                });
            }
            visit::visit_item_fn(self, f);
        }
    }
    let mut c = Collector {
        findings: Vec::new(),
        file: file_name.to_string(),
    };
    c.visit_file(file);
    c.findings
}

// --------------------------------------------------------------------------- comment extraction

/// `//` and `/* */` comments with their (1-based) line — string-aware so a
/// `//` inside a string literal or a char is not a comment. Python's comments
/// come from ruff tokens; Rust's from here; both feed `common`'s matching.
// code-health: ignore large-function the string-aware comment scanner is one linear pass — splitting it scatters the byte-state
pub fn rs_comment_lines(source: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let bytes = source.as_bytes();
    let mut i = 0usize;
    let mut line = 1usize;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                let start = i;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                out.push((line, source[start..i].to_string()));
                if i < bytes.len() {
                    i += 1; // consume the newline — counted once, here
                    line += 1;
                }
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                let start = i;
                let start_line = line;
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    if bytes[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                out.push((start_line, source[start..i].to_string()));
                continue;
            }
            b'"' => {
                // skip a string literal (with escapes); raw strings r".." /
                // r#".."# are handled by the 'r' arm below
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i = (i + 2).min(bytes.len()),
                        b'\n' => {
                            line += 1;
                            i += 1;
                        }
                        b if b == quote => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                continue;
            }
            b'\'' => {
                // a char literal is 'x' or an escape '\n' — a bare 'a is a
                // LIFETIME ('a, 'static) and must NOT start a literal scan
                // (it would swallow to the next apostrophe — losing every
                // comment in between)
                let is_char = (i + 2 < bytes.len() && bytes[i + 2] == b'\'')
                    || (i + 3 < bytes.len() && bytes[i + 1] == b'\\' && bytes[i + 3] == b'\'');
                if !is_char {
                    i += 1;
                    continue;
                }
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i = (i + 2).min(bytes.len()),
                        b'\n' => {
                            line += 1;
                            i += 1;
                        }
                        b if b == quote => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                continue;
            }
            b'r' if i + 2 < bytes.len() && bytes[i + 1] == b'"' => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 1;
                continue;
            }
            b'r' if i + 2 < bytes.len() && bytes[i + 1] == b'#' => {
                // hashed raw strings: r#"..."#, r##"..."## — count the hashes
                // and match the same count at the closing quote
                let mut hashes = 1usize;
                let mut j = i + 2;
                while j < bytes.len() && bytes[j] == b'#' {
                    hashes += 1;
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'"' {
                    j += 1;
                    while j + hashes < bytes.len() {
                        if bytes[j] == b'"' && bytes[j + 1..j + 1 + hashes].iter().all(|&b| b == b'#') {
                            break;
                        }
                        if bytes[j] == b'\n' {
                            line += 1;
                        }
                        j += 1;
                    }
                    i = (j + 1 + hashes).min(bytes.len());
                    continue;
                }
                i += 1; // not a raw string after all — a plain r
            }
            _ => i += 1,
        }
    }
    out
}

// --------------------------------------------------------------------------- duplicate skeletons

/// The structural fingerprint for the duplicate search — the Python layer's
/// shape (node kinds with names/literals/params collapsed) over syn's AST.
/// Token vocab is Rust-shaped (kind names + operator tokens); consistency
/// within the pool is what the dice threshold needs, not a shared vocab.
fn rs_skeleton(sig: &Signature, block: &Block) -> Vec<String> {
    let mut toks = vec!["Fn".to_string()];
    for _input in &sig.inputs {
        toks.push("A".to_string()); // params collapsed, like Python's "A"
    }
    for stmt in &block.stmts {
        skel_stmt(&mut toks, stmt);
    }
    toks
}

fn skel_stmt(toks: &mut Vec<String>, s: &Stmt) {
    match s {
        Stmt::Local(l) => {
            toks.push("Let".into());
            skel_pat(toks, &l.pat);
            if let Some(init) = &l.init {
                skel_expr(toks, &init.expr);
            }
        }
        Stmt::Expr(e, _) => {
            toks.push("ExprStmt".into());
            skel_expr(toks, e);
        }
        Stmt::Item(i) => match i {
            Item::Fn(f) => {
                toks.push("Fn".into());
                for s in &f.block.stmts {
                    skel_stmt(toks, s);
                }
            }
            Item::Struct(_) => toks.push("Struct".into()),
            Item::Enum(_) => toks.push("Enum".into()),
            Item::Impl(_) => toks.push("Impl".into()),
            Item::Use(_) => toks.push("Use".into()),
            Item::Mod(_) => toks.push("Mod".into()),
            Item::Const(_) => toks.push("Const".into()),
            Item::Static(_) => toks.push("Static".into()),
            Item::Trait(_) => toks.push("Trait".into()),
            Item::Type(_) => toks.push("Type".into()),
            _ => toks.push("Item".into()),
        },
        Stmt::Macro(_) => toks.push("MacroStmt".into()),
    }
}

// code-health: ignore large-function the skeleton mirrors syn's expr shapes — one exhaustive match is the point
fn skel_expr(toks: &mut Vec<String>, e: &Expr) {
    match e {
        Expr::Binary(b) => {
            toks.push(binop_token(&b.op).into());
            skel_expr(toks, &b.left);
            skel_expr(toks, &b.right);
        }
        Expr::Lit(_) => toks.push("C".into()),
        Expr::Path(_) => toks.push("N".into()),
        Expr::Call(c) => {
            toks.push("Call".into());
            skel_expr(toks, &c.func);
            for a in &c.args {
                skel_expr(toks, a);
            }
        }
        Expr::MethodCall(m) => {
            toks.push("MethodCall".into());
            skel_expr(toks, &m.receiver);
            for a in &m.args {
                skel_expr(toks, a);
            }
        }
        Expr::If(i) => {
            toks.push("If".into());
            skel_expr(toks, &i.cond);
            for s in &i.then_branch.stmts {
                skel_stmt(toks, s);
            }
            if let Some((_, else_e)) = &i.else_branch {
                skel_expr(toks, else_e);
            }
        }
        Expr::Match(m) => {
            toks.push("Match".into());
            skel_expr(toks, &m.expr);
            for arm in &m.arms {
                toks.push("Arm".into());
                skel_pat(toks, &arm.pat);
                skel_expr(toks, &arm.body);
            }
        }
        Expr::Return(r) => {
            toks.push("Return".into());
            if let Some(e) = &r.expr {
                skel_expr(toks, e);
            }
        }
        Expr::Break(_) => toks.push("Break".into()),
        Expr::Continue(_) => toks.push("Continue".into()),
        Expr::ForLoop(f) => {
            toks.push("For".into());
            skel_pat(toks, &f.pat);
            skel_expr(toks, &f.expr);
            for s in &f.body.stmts {
                skel_stmt(toks, s);
            }
        }
        Expr::While(w) => {
            toks.push("While".into());
            skel_expr(toks, &w.cond);
            for s in &w.body.stmts {
                skel_stmt(toks, s);
            }
        }
        Expr::Loop(l) => {
            toks.push("Loop".into());
            for s in &l.body.stmts {
                skel_stmt(toks, s);
            }
        }
        Expr::Closure(c) => {
            toks.push("Closure".into());
            skel_expr(toks, &c.body);
        }
        Expr::Field(f) => {
            toks.push("Field".into());
            skel_expr(toks, &f.base);
        }
        Expr::Index(i) => {
            toks.push("Index".into());
            skel_expr(toks, &i.expr);
            skel_expr(toks, &i.index);
        }
        Expr::Unary(u) => {
            toks.push(
                match u.op {
                    syn::UnOp::Neg(_) => "Neg",
                    syn::UnOp::Not(_) => "Not",
                    syn::UnOp::Deref(_) => "Deref",
                    _ => "UnOpX",
                }
                .into(),
            );
            skel_expr(toks, &u.expr);
        }
        Expr::Reference(_) => {
            toks.push("Ref".into());
        }
        Expr::Tuple(t) => {
            toks.push("Tuple".into());
            for e in &t.elems {
                skel_expr(toks, e);
            }
        }
        Expr::Array(a) => {
            toks.push("Array".into());
            for e in &a.elems {
                skel_expr(toks, e);
            }
        }
        Expr::Struct(s) => {
            toks.push("Struct".into());
            for f in &s.fields {
                skel_expr(toks, &f.expr);
            }
        }
        Expr::Cast(c) => {
            toks.push("Cast".into());
            skel_expr(toks, &c.expr);
        }
        Expr::Assign(a) => {
            toks.push("Assign".into());
            skel_expr(toks, &a.left);
            skel_expr(toks, &a.right);
        }
        Expr::Macro(_) => toks.push("Macro".into()),
        Expr::Paren(p) => skel_expr(toks, &p.expr),
        Expr::Range(_) => toks.push("Range".into()),
        Expr::Try(t) => {
            toks.push("Try".into());
            skel_expr(toks, &t.expr);
        }
        Expr::Async(a) => {
            toks.push("Async".into());
            for s in &a.block.stmts {
                skel_stmt(toks, s);
            }
        }
        Expr::Await(a) => {
            toks.push("Await".into());
            skel_expr(toks, &a.base);
        }
        Expr::Block(b) => {
            toks.push("Block".into());
            for s in &b.block.stmts {
                skel_stmt(toks, s);
            }
        }
        Expr::Let(l) => {
            toks.push("IfLet".into());
            skel_pat(toks, &l.pat);
            skel_expr(toks, &l.expr);
        }
        Expr::Yield(_) => toks.push("Yield".into()),
        Expr::Verbatim(_) => toks.push("Verbatim".into()),
        Expr::Group(g) => skel_expr(toks, &g.expr),
        Expr::Infer(_) => toks.push("Infer".into()),
        Expr::Const(c) => {
            toks.push("Const".into());
            for s in &c.block.stmts {
                skel_stmt(toks, s);
            }
        }
        Expr::RawAddr(a) => {
            toks.push("RawAddr".into());
            skel_expr(toks, &a.expr);
        }
        Expr::Repeat(r) => {
            toks.push("Repeat".into());
            skel_expr(toks, &r.expr);
            skel_expr(toks, &r.len);
        }
        Expr::TryBlock(t) => {
            toks.push("TryBlock".into());
            for s in &t.block.stmts {
                skel_stmt(toks, s);
            }
        }
        Expr::Unsafe(u) => {
            toks.push("Unsafe".into());
            for s in &u.block.stmts {
                skel_stmt(toks, s);
            }
        }
        _ => toks.push("X".into()), // syn is #[non_exhaustive] — unknown exprs collapse
    }
}

fn skel_pat(toks: &mut Vec<String>, p: &Pat) {
    match p {
        Pat::Ident(_) => toks.push("N".into()),
        Pat::Wild(_) => toks.push("Wild".into()),
        Pat::Lit(_) => toks.push("C".into()),
        Pat::Tuple(t) => {
            toks.push("PatTuple".into());
            for e in &t.elems {
                skel_pat(toks, e);
            }
        }
        Pat::Struct(s) => {
            toks.push("PatStruct".into());
            for f in &s.fields {
                skel_pat(toks, &f.pat);
            }
        }
        Pat::Type(t) => {
            toks.push("PatType".into());
            skel_pat(toks, &t.pat);
        }
        Pat::Or(o) => {
            toks.push("PatOr".into());
            for c in &o.cases {
                skel_pat(toks, c);
            }
        }
        Pat::Slice(s) => {
            toks.push("PatSlice".into());
            for e in &s.elems {
                skel_pat(toks, e);
            }
        }
        Pat::Reference(r) => {
            toks.push("PatRef".into());
            skel_pat(toks, &r.pat);
        }
        Pat::Const(_) => toks.push("PatConst".into()),
        Pat::Paren(p) => skel_pat(toks, &p.pat),
        Pat::Verbatim(_) => toks.push("PatVerbatim".into()),
        Pat::Macro(_) => toks.push("PatMacro".into()),
        Pat::Rest(_) => toks.push("PatRest".into()),
        Pat::Path(_) => toks.push("N".into()),
        Pat::Range(_) => toks.push("PatRange".into()),
        Pat::TupleStruct(t) => {
            toks.push("PatTupleStruct".into());
            for e in &t.elems {
                skel_pat(toks, e);
            }
        }
        _ => toks.push("PatX".into()),
    }
}

fn binop_token(op: &BinOp) -> &'static str {
    use syn::BinOp::*;
    match op {
        Add(_) => "Add",
        Sub(_) => "Sub",
        Mul(_) => "Mul",
        Div(_) => "Div",
        Rem(_) => "Rem",
        And(_) => "And",
        Or(_) => "Or",
        BitXor(_) => "BitXor",
        BitAnd(_) => "BitAnd",
        BitOr(_) => "BitOr",
        Shl(_) => "Shl",
        Shr(_) => "Shr",
        Eq(_) => "Eq",
        Lt(_) => "Lt",
        Le(_) => "Le",
        Ne(_) => "Ne",
        Ge(_) => "Ge",
        Gt(_) => "Gt",
        AddAssign(_) => "AddAssign",
        SubAssign(_) => "SubAssign",
        MulAssign(_) => "MulAssign",
        DivAssign(_) => "DivAssign",
        RemAssign(_) => "RemAssign",
        BitXorAssign(_) => "BitXorAssign",
        BitAndAssign(_) => "BitAndAssign",
        BitOrAssign(_) => "BitOrAssign",
        ShlAssign(_) => "ShlAssign",
        ShrAssign(_) => "ShrAssign",
        _ => "Op",
    }
}

// --------------------------------------------------------------------------- module graph

/// The module->file map and the file->[module files] adjacency for import
/// cycles. `files` is the repo-relative scan set; `mod_decls` maps each file
/// to its top-level `mod` declarations. Crate roots are files named main.rs
/// or lib.rs (a workspace's roots are all covered).
pub fn module_graph(
    files: &[String],
    mod_decls: &HashMap<String, Vec<(String, bool)>>,
    uses: &HashMap<String, Vec<String>>,
) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut graph: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    let scan_set: std::collections::HashSet<String> = files.iter().cloned().collect();
    // module path -> file rel, scoped per root to avoid workspace collisions
    // (two crates with a module named "a" would collide with unqualified paths).
    // Keys are prefixed with the root directory (e.g. "crate_a/src::a").
    let mut module_map: HashMap<String, String> = HashMap::new();
    // Per-root top-level names for bare `use a::b` resolution
    let mut top_level_by_root: HashMap<String, Vec<String>> = HashMap::new();
    for root in files {
        if !(root.ends_with("main.rs") || root.ends_with("lib.rs")) {
            continue;
        }
        let dir = root
            .rsplit('/')
            .next()
            .map(|_| root[..root.rfind('/').map_or(0, |i| i)].to_string());
        let dir = dir.unwrap_or_default();
        let prefix = dir.clone(); // scope prefix for this root
        let root_decls = top_level_by_root.entry(root.clone()).or_default();
        let mut stack: Vec<(String, String)> = Vec::new(); // (module path, dir)
        if let Some(decls) = mod_decls.get(root) {
            for (name, _inline) in decls {
                root_decls.push(name.clone());
                let child = if dir.is_empty() {
                    name.clone()
                } else {
                    format!("{dir}/{name}")
                };
                stack.push((child, name.clone()));
            }
        }
        while let Some((child_dir, path)) = stack.pop() {
            let candidates = [format!("{child_dir}.rs"), format!("{child_dir}/mod.rs")];
            let found = candidates.into_iter().find(|c| scan_set.contains(c));
            if let Some(file) = found {
                // Qualify the key with the root's directory prefix
                let key = if prefix.is_empty() {
                    path.clone()
                } else {
                    format!("{prefix}::{path}")
                };
                module_map.insert(key, file.clone());
                graph.entry(file.clone()).or_default();
                if let Some(decls) = mod_decls.get(&file) {
                    for (name, _) in decls {
                        stack.push((format!("{child_dir}/{name}"), format!("{path}::{name}")));
                    }
                }
            }
        }
    }
    // top-level module names for bare `use a::b` resolution (qualified by root)
    let top_level: std::collections::HashSet<String> = files
        .iter()
        .filter(|f| f.ends_with("main.rs") || f.ends_with("lib.rs"))
        .filter_map(|f| top_level_by_root.get(f))
        .flatten()
        .cloned()
        .collect();
    for file in files {
        let Some(paths) = uses.get(file) else { continue };
        let mut out_edges = Vec::new();
        for path in paths {
            let target = resolve_use(path, file, &module_map, &top_level);
            if let Some(t) = target {
                if t != *file {
                    out_edges.push(t);
                }
            }
        }
        if !out_edges.is_empty() {
            out_edges.sort();
            out_edges.dedup();
            graph.insert(file.clone(), out_edges);
        }
    }
    graph
}

/// Resolve one raw use path to a scanned file rel (the module that owns the
/// imported name), or None when it is external or unresolvable.
fn resolve_use(
    path: &str,
    file: &str,
    module_map: &HashMap<String, String>,
    top_level: &std::collections::HashSet<String>,
) -> Option<String> {
    let (rest, is_crate) = if let Some(r) = path.strip_prefix("crate::") {
        (r.to_string(), true)
    } else if path.starts_with("::") {
        (path.trim_start_matches(':').to_string(), true)
    } else {
        (path.to_string(), false)
    };
    if !is_crate {
        if let Some(r) = rest.strip_prefix("super::") {
            // the file's own module path, up one level, plus the rest
            let own = file_module_path(file, module_map)?;
            let parent = match own.rfind("::") {
                Some(i) => &own[..i],
                None => "",
            };
            let joined = if parent.is_empty() {
                r.to_string()
            } else {
                format!("{parent}::{r}")
            };
            return resolve_dotted(&joined, module_map);
        }
        if let Some(r) = rest.strip_prefix("self::") {
            let own = file_module_path(file, module_map)?;
            let joined = if own.is_empty() {
                r.to_string()
            } else {
                format!("{own}::{r}")
            };
            return resolve_dotted(&joined, module_map);
        }
        // bare path: edition-2018 absolute — a top-level local module, else external
        let first = rest.split("::").next().unwrap_or("");
        if first.is_empty() || !top_level.contains(first) {
            return None;
        }
    }
    resolve_dotted(&rest, module_map)
}

fn resolve_dotted(path: &str, module_map: &HashMap<String, String>) -> Option<String> {
    let parts: Vec<&str> = path.split("::").collect();
    for cut in (1..parts.len()).rev() {
        let candidate = parts[..cut].join("::");
        // Try the exact key (which may include a root-scope prefix)
        if let Some(f) = module_map.get(&candidate) {
            return Some(f.clone());
        }
    }
    None
}

fn file_module_path(file: &str, module_map: &HashMap<String, String>) -> Option<String> {
    let _ = file;
    module_map.iter().find(|(_, f)| *f == file).map(|(p, _)| p.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(src: &str) -> Vec<Finding> {
        scan_source(src, "prod_mod.rs", true).findings
    }

    fn scan_cc(src: &str) -> Vec<FnCc> {
        scan_source(src, "prod_mod.rs", true).cc
    }

    fn has_kind(fs: &[Finding], k: &str) -> bool {
        fs.iter().any(|f| f.kind == k)
    }

    #[test]
    fn cc_if_else_if_counts_elifs_not_else() {
        let src = "fn f(a: u32, b: u32) -> u32 {\n    if a > 0 { return 1; } else if b > 0 { return 2; } else { return 3; }\n}\n";
        assert_eq!(scan_cc(src)[0].cc, 3); // if + else-if; trailing else does NOT count
    }

    #[test]
    fn cc_match_arms_minus_wildcard() {
        let src =
            "fn f(x: u32) -> u32 {\n    match x {\n        1 => 10,\n        2 => 20,\n        _ => 0,\n    }\n}\n";
        assert_eq!(scan_cc(src)[0].cc, 3); // 2 arms + base; the wildcard does NOT count
    }

    #[test]
    fn cc_loops_boolop_assert_closure() {
        let src = "fn f(xs: &[u32], a: bool, b: bool) -> u32 {\n    let mut n = 0;\n    for x in xs {\n        if a && b { n += 1; }\n    }\n    while n < 10 { n += 1; }\n    loop { n += 1; if n > 20 { break; } }\n    assert!(a || b);\n    let g = |y: u32| if y > 1 { y } else { 0 };\n    n + g(1)\n}\n";
        // for(1) if(1) &&(1) while(1) loop(1) if(1) break(0) assert(1) closure(+0) ternary(1) + base
        assert_eq!(scan_cc(src)[0].cc, 8);
    }

    #[test]
    fn cc_nested_fn_excluded_from_outer() {
        let src = "fn outer() -> u32 {\n    fn inner(x: u32) -> u32 { if x > 0 { 1 } else { 0 } }\n    if inner(1) > 0 { 1 } else { 0 }\n}\n";
        // radon's fn_map holds module-level fns only — the nested fn's
        // decisions count for ITS scope (closures rule) but never reach cc
        let cc = scan_cc(src);
        assert_eq!(cc.len(), 1);
        assert_eq!(cc[0].cc, 2); // outer: if(1) + base
    }

    // ------------------------------------------------------------- magic
    #[test]
    fn magic_operand_and_call_arg_found() {
        let src = "fn f() -> u32 { let x = 3 * 60; foo(300); x }\n";
        let fs = scan(src);
        assert_eq!(fs.iter().filter(|f| f.kind == "magic-number").count(), 3); // 3, 60, 300 — only 0/1/2 are trivial
    }

    #[test]
    fn magic_return_value_and_const_def_not_found() {
        let src = "fn f() -> u32 { return 60; }\nconst LIMIT: u32 = 60;\n";
        let fs = scan(src);
        assert!(!has_kind(&fs, "magic-number"));
    }

    #[test]
    fn magic_trivial_literals_and_suffixes_skipped() {
        let src = "fn f(x: u32) -> u32 { x * 0usize + 1 + 2 }\n";
        assert!(!has_kind(&scan(src), "magic-number"));
    }

    #[test]
    fn magic_skipped_in_test_code() {
        let src = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { assert_eq!(foo(), 60); }\n}\nfn f(x: u32) -> u32 { x + 2 }\n";
        let fs = scan(src);
        assert!(!has_kind(&fs, "magic-number")); // 60 lives in test code; 2 is trivial in prod
    }

    // ------------------------------------------------------------- noop
    #[test]
    fn noop_bare_path_and_binary_flagged() {
        let src = "fn f(x: u32, y: u32) {\n    x;\n    x + y;\n    foo();\n    let z = 1;\n    z += 1;\n    println!(\"hi\");\n    x;\n}\n";
        let fs = scan(src);
        assert_eq!(fs.iter().filter(|f| f.kind == "noop-statement").count(), 3);
        // x; x+y; x; — calls/macros/assigns pass
    }

    #[test]
    fn noop_if_and_match_statements_flagged() {
        let src = "fn f(x: u32) {\n    if x > 1 { 1 } else { 0 };\n    match x { _ => 0 };\n}\n";
        assert_eq!(scan(src).iter().filter(|f| f.kind == "noop-statement").count(), 2);
    }

    // ------------------------------------------------------------- unreachable
    #[test]
    fn unreachable_after_return_and_panic() {
        let src = "fn f(x: u32) -> u32 {\n    if x > 0 {\n        return 1;\n        foo();\n    }\n    panic!(\"no\");\n    bar();\n}\n";
        let fs = scan(src);
        assert_eq!(fs.iter().filter(|f| f.kind == "unreachable").count(), 2);
    }

    #[test]
    fn unreachable_nested_fn_body_does_not_leak() {
        let src = "fn outer() -> u32 {\n    fn inner() -> u32 { return 1; }\n    inner()\n}\n";
        assert!(!has_kind(&scan(src), "unreachable")); // inner's return ends only inner's block
    }

    // ------------------------------------------------------------- closures -> latent class
    #[test]
    fn closures_latent_class_with_cc_gate() {
        // nested FN definitions (not closures — those are idiomatic Rust)
        let mut body = String::from("fn f() -> u32 {\n    fn a() -> u32 { 1 }\n    fn b() -> u32 { 2 }\n");
        for i in 0..18 {
            body.push_str(&format!("    if {i} > 0 {{ a(); }} else {{ b(); }}\n"));
        }
        body.push_str("    a() + b()\n}\n");
        assert!(has_kind(&scan(&body), "closures"));
    }

    #[test]
    fn closures_closures_are_not_counted() {
        // a fn full of idiomatic local closures is NOT a latent class
        let src = "fn f(xs: &[u32]) -> u32 {\n    let a = |x: u32| x + 1;\n    let b = |x: u32| x * 2;\n    xs.iter().map(a).map(b).sum()\n}\n";
        assert!(!has_kind(&scan(src), "closures"));
    }

    #[test]
    fn closures_thin_fn_passes() {
        let src = "fn f() -> u32 {\n    fn a() -> u32 { 1 }\n    fn b() -> u32 { 2 }\n    a() + b()\n}\n";
        assert!(!has_kind(&scan(src), "closures")); // 2 inner but cc < 15 and span < 60
    }

    // ------------------------------------------------------------- large-function
    #[test]
    fn large_function_over_120_lines() {
        let mut src = String::from("fn f() -> u32 {\n    let mut n = 0;\n");
        for i in 0..130 {
            src.push_str(&format!("    n += {i};\n"));
        }
        src.push_str("    n\n}\n");
        assert!(has_kind(&scan(&src), "large-function"));
    }

    // ------------------------------------------------------------- vague-name + record-shape
    #[test]
    fn vague_role_suffix_struct_with_weight() {
        // a role-suffixed struct with 6+ methods is load-bearing — the name hides it
        let src = "struct ConfigManager { a: u32, b: u32, c: u32 }\nimpl ConfigManager {\n    fn m1(&self) -> u32 { self.a }\n    fn m2(&self) -> u32 { self.b }\n    fn m3(&self) -> u32 { self.c }\n    fn m4(&self) -> u32 { self.a + self.b }\n    fn m5(&self) -> u32 { self.b + self.c }\n    fn m6(&self) -> u32 { self.a + self.c }\n}\n";
        assert!(has_kind(&scan(src), "vague-name"));
    }

    #[test]
    fn record_shape_boundary_finding() {
        let src =
            "struct BigRecord { a: u32, b: u32, c: u32, d: u32, e: u32 }\nfn consume(r: &BigRecord) -> u32 { r.a }\n";
        let fs = scan(src);
        assert!(has_kind(&fs, "record-shape"));
        let f = fs.iter().find(|f| f.kind == "record-shape").unwrap();
        assert!(f.message.contains("consume"));
    }

    #[test]
    fn record_shape_with_methods_passes() {
        let src = "struct Big { a: u32, b: u32, c: u32, d: u32, e: u32 }\nimpl Big { fn total(&self) -> u32 { self.a } }\nfn use_big(b: &Big) -> u32 { b.total() }\n";
        assert!(!has_kind(&scan(src), "record-shape"));
    }

    // ------------------------------------------------------------- suppressions
    #[test]
    fn suppression_with_why_exempts_and_whyless_is_finding() {
        let src =
            "fn f() -> u32 {\n    // code-health: ignore magic-number the gate threshold\n    let x = 3 * 60;\n}\n";
        let fs = scan(src);
        assert!(!has_kind(&fs, "magic-number"));
        let src2 = "fn f() -> u32 {\n    // code-health: ignore magic-number\n    let x = 3 * 60;\n}\n";
        assert!(has_kind(&scan(src2), "suppression"));
    }

    #[test]
    fn allow_without_reason_is_finding() {
        let src = "#[allow(clippy::too_many_lines)]\nfn f() {}\n";
        assert!(has_kind(&scan(src), "allow-reason"));
    }

    #[test]
    fn allow_with_two_line_reason_passes() {
        // the checks.rs case: a reason comment spanning two lines above the attr
        let src = "// reason: first line —\n// second line\n#[allow(clippy::too_many_lines)]\nfn f() {}\n";
        let fs = scan(src);
        assert!(!has_kind(&fs, "allow-reason"), "{:?}", fs);
    }

    #[test]
    fn allow_with_reason_passes() {
        let src = "// the gate intentionally allows this\n#[allow(clippy::too_many_lines)]\nfn f() {}\n";
        let fs = scan(src);
        assert!(!has_kind(&fs, "allow-reason"));
    }

    // ------------------------------------------------------------- test rules
    #[test]
    fn ignored_test_is_skipif_finding() {
        let src = "#[cfg(test)]\nmod tests {\n    #[test]\n    #[ignore]\n    fn slow() {}\n    #[test]\n    fn fast() {}\n}\n";
        let fs = scan(src);
        assert!(has_kind(&fs, "skipif"));
        assert_eq!(fs.iter().filter(|f| f.kind == "skipif").count(), 1);
    }

    // ------------------------------------------------------------- comments
    #[test]
    fn comment_extraction_handles_hashed_raw_strings() {
        // r#"..."# content may contain " and // — neither is a comment
        let src = "let a = r#\"not // a comment \" inside\"#;\n// real comment\nlet b = r##\"has # in it\"##;\n";
        let lines = rs_comment_lines(src);
        let texts: Vec<String> = lines.iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(texts.len(), 1);
        assert!(texts[0].contains("real comment"));
    }

    #[test]
    fn comment_extraction_is_string_aware() {
        let src = "// real comment\nlet s = \"// not a comment\";\nlet c = '/'; // trailing\n/* block\ncomment */\n";
        let lines = rs_comment_lines(src);
        let texts: Vec<String> = lines.iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(texts.len(), 3); // the quoted "// not a comment" is not one
        assert!(texts.iter().any(|t| t.contains("real comment")));
        assert!(texts.iter().any(|t| t.contains("trailing")));
        assert!(texts.iter().any(|t| t.contains("block")));
        assert!(!texts.iter().any(|t| t.contains("not a comment")));
    }

    // ------------------------------------------------------------- duplicates
    #[test]
    fn duplicate_detection_across_rust_fns() {
        let a =
            "fn alpha(x: u32) -> u32 {\n    let mut n = x;\n    for i in 0..10 {\n        n += i;\n    }\n    n\n}\n";
        let b =
            "fn beta(y: u32) -> u32 {\n    let mut m = y;\n    for j in 0..10 {\n        m += j;\n    }\n    m\n}\n";
        let sa = scan_source(a, "a.rs", true);
        let sb = scan_source(b, "b.rs", true);
        let mut pool: Vec<common::SkeletonFn> = Vec::new();
        pool.extend(sa.skeletons.iter().map(|s| common::SkeletonFn {
            rel: "a.rs".into(),
            name: s.name.clone(),
            line: s.line,
            skeleton: s.skeleton.clone(),
        }));
        pool.extend(sb.skeletons.iter().map(|s| common::SkeletonFn {
            rel: "b.rs".into(),
            name: s.name.clone(),
            line: s.line,
            skeleton: s.skeleton.clone(),
        }));
        let dups = crate::checks::duplicate_findings(&pool);
        assert_eq!(dups.len(), 1, "{dups:?}");
        assert!(dups[0].message.contains("beta"));
    }

    // ------------------------------------------------------------- module graph
    #[test]
    fn module_graph_finds_import_cycle() {
        let files = vec!["main.rs".to_string(), "a.rs".to_string(), "b.rs".to_string()];
        let mut mods: HashMap<String, Vec<(String, bool)>> = HashMap::new();
        mods.insert("main.rs".into(), vec![("a".into(), false), ("b".into(), false)]);
        mods.insert("a.rs".into(), vec![]);
        mods.insert("b.rs".into(), vec![]);
        let mut uses: HashMap<String, Vec<String>> = HashMap::new();
        uses.insert("a.rs".into(), vec!["crate::b::helper".into()]);
        uses.insert("b.rs".into(), vec!["crate::a::helper".into()]);
        let graph = module_graph(&files, &mods, &uses);
        assert_eq!(graph.get("a.rs").map(|v| v.as_slice()), Some(&["b.rs".to_string()][..]));
        assert_eq!(graph.get("b.rs").map(|v| v.as_slice()), Some(&["a.rs".to_string()][..]));
        let fs = crate::graph_families::cycle_findings_for(&graph);
        assert_eq!(fs.len(), 1);
        assert!(fs[0].message.contains("a.rs"));
        assert!(fs[0].message.contains("b.rs"));
    }

    #[test]
    fn module_graph_skips_external_crates() {
        let files = vec!["main.rs".to_string(), "a.rs".to_string()];
        let mut mods: HashMap<String, Vec<(String, bool)>> = HashMap::new();
        mods.insert("main.rs".into(), vec![("a".into(), false)]);
        mods.insert("a.rs".into(), vec![]);
        let mut uses: HashMap<String, Vec<String>> = HashMap::new();
        uses.insert("a.rs".into(), vec!["serde::Deserialize".into()]);
        let graph = module_graph(&files, &mods, &uses);
        assert!(graph.values().all(|v| v.is_empty())); // serde is not a local module — no edges
    }

    #[test]
    fn swallow_let_underscore_discards_calls() {
        let fs = scan("fn f() {\n    let _ = foo();\n}\n");
        assert!(has_kind(&fs, "swallow"));
        let fs2 = scan("fn f() {\n    let _ = x;\n}\n");
        assert!(!has_kind(&fs2, "swallow")); // plain-path moves are not swallows
    }

    #[test]
    fn swallow_skipped_in_test_code() {
        let fs = scan("#[cfg(test)]\nmod t {\n    #[test]\n    fn f() {\n        let _ = foo();\n    }\n}\n");
        assert!(!has_kind(&fs, "swallow"));
    }

    #[test]
    fn debug_artifact_dbg_and_unwrap() {
        let fs = scan("fn f() {\n    dbg!(x);\n    let y = foo().unwrap();\n}\n");
        assert!(has_kind(&fs, "debug-artifact"));
        let fs2 = scan("fn f() {\n    let y = foo();\n}\n");
        assert!(!has_kind(&fs2, "debug-artifact"));
    }

    #[test]
    fn debug_artifact_skipped_in_tests() {
        let fs = scan("#[cfg(test)]\nmod t {\n    #[test]\n    fn f() {\n        dbg!(x);\n        let y = foo().unwrap();\n    }\n}\n");
        assert!(!has_kind(&fs, "debug-artifact"));
    }

    #[test]
    fn boolean_literal_argument_is_found() {
        let fs = scan("fn f() {\n    connect(\"host\", true);\n}\n");
        assert!(has_kind(&fs, "boolean-arg"));
        let fs2 = scan("fn f() {\n    let retry = true;\n    connect(\"host\", retry);\n}\n");
        assert!(!has_kind(&fs2, "boolean-arg"));
    }

    #[test]
    fn long_parameter_list_over_five() {
        let fs = scan("fn f(a: i32, b: i32, c: i32, d: i32, e: i32, g: i32) {}\n");
        assert!(has_kind(&fs, "long-param-list"));
        let fs2 = scan("fn f(a: i32, b: i32, c: i32, d: i32, e: i32) {}\n");
        assert!(!has_kind(&fs2, "long-param-list"));
    }

    #[test]
    fn no_assert_test_is_flagged() {
        let fs = scan("fn helper() {}\n#[test]\nfn t() {}\n");
        assert!(has_kind(&fs, "no-assert-test"));
        let ok = scan("#[test]\nfn t() {\n    assert_eq!(1, 1);\n}\n");
        assert!(!has_kind(&ok, "no-assert-test"));
        let panic = scan("#[test]\n#[should_panic]\nfn t() {}\n");
        assert!(!has_kind(&panic, "no-assert-test"));
        let ignored = scan("#[test]\n#[ignore]\nfn t() {}\n");
        assert!(!has_kind(&ignored, "no-assert-test"));
    }

    #[test]
    fn strewing_free_fns_sharing_struct_param() {
        let fs = scan("struct S {}\nfn a(s: S) {}\nfn b(s: S) {}\nfn c(s: S) {}\n");
        assert!(has_kind(&fs, "strewing"));
        let fs2 = scan("struct S {}\nfn a(s: S) {}\nfn b(s: S) {}\n");
        assert!(!has_kind(&fs2, "strewing"));
    }

    #[test]
    fn global_state_statics_flagged() {
        let fs = scan("static mut X: i32 = 0;\n");
        assert!(has_kind(&fs, "global-state"));
        let fs2 = scan("static X: Mutex<i32> = Mutex::new(0);\n");
        assert!(has_kind(&fs2, "global-state"));
        let ok = scan("static X: i32 = 0;\n");
        assert!(!has_kind(&ok, "global-state"));
    }

    #[test]
    fn fakefs_literal_path_in_test() {
        let fs = scan("#[test]\nfn t() {\n    std::fs::write(\"out.txt\", b\"x\");\n}\n");
        assert!(has_kind(&fs, "fakefs"));
        let ok = scan(
            "#[test]\nfn t() {\n    let dir = std::env::temp_dir();\n    std::fs::write(dir.join(\"x\"), b\"x\");\n}\n",
        );
        assert!(!has_kind(&ok, "fakefs"));
        let prod = scan("fn f() {\n    std::fs::write(\"out.txt\", b\"x\");\n}\n");
        assert!(!has_kind(&prod, "fakefs")); // test-only family
    }

    #[test]
    fn stale_suppression_flagged() {
        let fs = scan("// code-health: ignore magic-number this line has nothing\nfn f() {}\n");
        assert!(has_kind(&fs, "stale-suppression"));
        let ok = scan("fn f() {\n    let x = 3 * 60; // code-health: ignore magic-number the gate threshold\n}\n");
        assert!(!has_kind(&ok, "stale-suppression"));
    }

    #[test]
    fn detached_method_rust_flagged() {
        let fs = scan("struct S {}\nimpl S {\n    fn m(&self, x: i32) -> i32 {\n        x + 1\n    }\n}\n");
        assert!(has_kind(&fs, "detached-method"));
        let ok = scan("struct S {}\nimpl S {\n    fn m(&self, x: i32) -> i32 {\n        self.x + x\n    }\n}\n");
        assert!(!has_kind(&ok, "detached-method"));
        let assoc = scan("struct S {}\nimpl S {\n    fn m(x: i32) -> i32 {\n        x\n    }\n}\n");
        assert!(!has_kind(&assoc, "detached-method")); // no receiver — associated fn
    }

    #[test]
    fn detached_method_self_inside_macro_counts() {
        // write! hides `self` in a macro token group — the probe must descend
        let fs = scan("struct S { x: i32 }\nimpl std::fmt::Display for S {\n    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {\n        write!(f, \"{}\", self.x)\n    }\n}\n");
        assert!(!has_kind(&fs, "detached-method"));
    }

    #[test]
    fn rust_skeleton_shapes() {
        let src = "fn f(x: u32) -> u32 { if x > 0 { x + 1 } else { x } }\n";
        let file = syn::parse_file(src).unwrap();
        let Item::Fn(f) = &file.items[0] else {
            panic!("expected fn")
        };
        let skel = rs_skeleton(&f.sig, &f.block);
        assert_eq!(skel[0], "Fn");
        assert_eq!(skel[1], "A");
        assert!(skel.contains(&"If".to_string()));
        assert!(skel.contains(&"Gt".to_string()));
        assert!(skel.contains(&"C".to_string()));
        assert!(skel.contains(&"N".to_string()));
    }
}

// lucidlint: ignore-file complexity the ruff visitor walkers are parity-locked dispatch tables —
// match-arm count, not branching; keep NEW functions under cc 15

//! lucidlint — the Rust scan core for the deterministic lucidlint gate.
//!
//! Phase 1 of the port: parsing + the pure-walk checks. The finding schema is
//! deliberately language-neutral (file/line/function/kind/severity/message) —
//! JS and other languages plug in later with their own parsers behind the
//! same schema.
//!
//! Architecture: EVERY function (methods and nested included) becomes a scan
//! scope for the walk-based checks (unreachable, imports, magic numbers,
//! no-op statements) with innermost-function attribution; only module-level
//! functions report cyclomatic complexity, exactly as radon does (nested
//! functions and class bodies contribute no decisions to their parent).

use rayon::prelude::*;
use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
use ruff_python_ast::{AnyNodeRef, Expr, ModModule, Stmt, StmtIf};
use ruff_python_parser::{parse_module, Parsed};
use ruff_text_size::Ranged;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

mod checks;
mod common;
mod config;
mod docs;
mod fix;
mod graph_families;
mod lsp;
mod rules_gen;
mod rustscan;
use checks::Q;
use checks::*;

/// One open function scope: its decision count and the names it returns
/// (for the swallow analysis).
pub struct FnScope {
    /// Names this function returns (for the swallow analysis).
    pub returned: HashSet<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Finding {
    file: String,
    line: usize,
    function: String,
    kind: String,
    severity: String,
    message: String,
}

/// One function's cyclomatic complexity — radon-equivalent counting.
#[derive(Serialize, Clone)]
pub struct FnCc {
    file: String,
    function: String,
    line: usize,
    cc: u32,
    /// The body's SHAPE for the complexity message routing: "dispatch",
    /// "rules", or "plain" (see python_fn_shape / rust_fn_shape).
    pub shape: &'static str,
    /// The dispatch selector or rule-battery accumulator name.
    pub shape_detail: String,
}

#[derive(Default)]
struct ScanState<'a> {
    file: &'a str,
    source: &'a str,
    findings: Vec<Finding>,
    cc: Vec<FnCc>,
    /// (name, start line) of the innermost function — attribution.
    current_fn: Option<(String, usize)>,
    /// Open function scopes (innermost last): decisions + the enclosing
    /// function's returned names (for the swallow analysis).
    fn_stack: Vec<FnScope>,
    /// Class nesting: class bodies contribute no decisions (radon sub-visitor).
    in_class: u32,
    /// Chains the latent-visitor rule claimed — conditional-polymorphism
    /// must not double-flag them (one ruling per chain, or the agent thrashes
    /// between implementing the visitor and the polymorphic methods).
    claimed_dispatch: std::collections::HashSet<usize>,
    /// Parent chain for the magic-number position check — exprs plus the
    /// non-expr layers (stmt, keyword) that break the direct-parent link.
    parent_stack: Vec<ParentEntry>,
    /// Module-scope container names (List/Dict/Set) — mutations of these
    /// inside functions are global-state findings.
    module_mutables: HashSet<String>,
    /// Module containers whose literal was flagged (non-constant) — their
    /// in-function mutations are not double-reported (Python's `flagged`).
    module_flagged: HashSet<String>,
    /// Module-level function definitions (name, line) — non-test files.
    defs: Vec<(String, usize)>,
    /// Every referenced name (Name nodes + import aliases) in this file.
    refs: HashSet<String>,
    /// String literal values (prod files only).
    strings: Vec<String>,
    /// Module-level function names with decorators (framework-registered).
    decorated: HashSet<String>,
    /// Duplicate candidates with their structural skeletons.
    skeletons: Vec<SkeletonFn>,
    /// File is under a test path — reference-scan split + skeleton skip.
    is_test: bool,
}

fn line_of(source: &str, offset: ruff_text_size::TextSize) -> usize {
    1 + source[..offset.to_usize()].bytes().filter(|&b| b == b'\n').count()
}

enum ParentEntry {
    Expr(Expr),
    Stmt,
    Keyword,
}

impl<'a> SourceOrderVisitor<'a> for ScanState<'a> {
    // lucidlint: ignore large-function the ruff visitor dispatches statement kinds in CPython's visit order
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        // module scope = no open function scope and no class nesting (the
        // FunctionDef arm walks bodies manually, so parent_stack is not the
        // reliable signal here)
        let module_level = self.fn_stack.is_empty() && self.in_class == 0;
        match stmt {
            Stmt::FunctionDef(f) => {
                let module_level = self.fn_stack.is_empty() && self.in_class == 0;
                let was_fn = self.current_fn.take();
                self.current_fn = Some((f.name.to_string(), line_of(self.source, f.range.start())));
                self.fn_stack.push(FnScope {
                    returned: returned_names(&f.body),
                });
                self.unreachable_check(&f.body);
                shadow_findings(self, stmt);
                let source = self.source;
                long_param_list_findings(self, f, source);
                // signature exprs feed the reference scan (ast.walk covers
                // decorators, parameter annotations/defaults, and returns)
                for d in &f.decorator_list {
                    self.visit_expr(&d.expression);
                }
                for pwd in f
                    .parameters
                    .posonlyargs
                    .iter()
                    .chain(&f.parameters.args)
                    .chain(&f.parameters.kwonlyargs)
                {
                    if let Some(a) = &pwd.parameter.annotation {
                        self.visit_expr(a.as_ref());
                    }
                    if let Some(d) = &pwd.default {
                        self.visit_expr(d.as_ref());
                    }
                }
                for p in [&f.parameters.vararg, &f.parameters.kwarg].into_iter().flatten() {
                    if let Some(a) = &p.annotation {
                        self.visit_expr(a.as_ref());
                    }
                }
                if let Some(r) = &f.returns {
                    self.visit_expr(r.as_ref());
                }
                for s in &f.body {
                    self.visit_stmt(s);
                }
                // the push/pop invariant is per-function; a missing scope is a
                // walker bug, not a crash — bail out of this function's checks
                let Some(_scope) = self.fn_stack.pop() else {
                    return;
                };
                // The radon-equivalent CC comes from the radonc crate (its rules
                // match radon 6.0.1 exactly; the visitor still tracks decisions
                // for the closures/latent-class gate).
                let cc = radonc::function_cc(f) as u32;
                // repo-wide collections: module-level defs + duplicate
                // candidates (the def line is the NAME's line — ruff's
                // FunctionDef range starts at the decorator)
                let def_line = line_of(self.source, f.name.range().start());
                if module_level && !self.is_test {
                    self.defs.push((f.name.to_string(), def_line));
                    if !f.decorator_list.is_empty() {
                        self.decorated.insert(f.name.to_string());
                    }
                }
                let span = (line_of(self.source, f.range().end()) - line_of(self.source, f.range().start())) as u32;
                // the Python gate reads cc from radon's fn_map, which only
                // holds module-level functions — methods/nested get cc = 0
                let gate_cc = if module_level { cc } else { 0 };
                closure_findings(self, stmt, gate_cc, span);
                // a method that never touches its receiver does not belong in
                // the class (the inverse of record-shape/strewing)
                if self.in_class > 0 {
                    detached_method_findings(self, f, source);
                }
                if module_level {
                    // the def line (name range), not the decorator — radon's
                    // fn.lineno; the parity test's decorated-line offset
                    // normalization existed because of this difference
                    let (shape, shape_detail) = python_fn_shape(&f.body);
                    self.cc.push(FnCc {
                        file: self.file.to_string(),
                        function: f.name.to_string(),
                        line: def_line,
                        cc,
                        shape,
                        shape_detail,
                    });
                }
                self.current_fn = was_fn;
                return;
            }
            Stmt::ClassDef(_) => {
                // class bodies: no decisions, but walked (imports/exprs inside
                // still get visited with their function attribution)
                self.in_class += 1;
                walk_stmt(self, stmt);
                self.in_class -= 1;
                return;
            }
            Stmt::Expr(e) => self.noop_check(&e.value),
            Stmt::Import(imp) => {
                self.import_checks(stmt);
                for a in &imp.names {
                    self.refs.insert(a.name.to_string());
                }
            }
            Stmt::ImportFrom(imp) => {
                self.import_checks(stmt);
                for a in &imp.names {
                    self.refs.insert(a.name.to_string());
                }
            }
            Stmt::Try(_) => except_findings(self, stmt),
            Stmt::Global(_) => {
                let module_level = false; // globals inside a function
                global_state_findings(self, stmt, module_level);
            }
            Stmt::Assign(_) => {
                global_state_findings(self, stmt, module_level);
                shadow_findings(self, stmt);
                mutation_findings(self, stmt);
            }
            Stmt::AugAssign(_) | Stmt::AnnAssign(_) | Stmt::Delete(_) => {
                global_state_findings(self, stmt, module_level);
                mutation_findings(self, stmt);
            }
            _ => {}
        }
        // CC (cyclomatic complexity) comes from the radonc crate — the scan
        // walk itself no longer counts decisions.
        self.parent_stack.push(ParentEntry::Stmt);
        walk_stmt(self, stmt);
        self.parent_stack.pop();
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(n) => {
                self.refs.insert(n.id.to_string());
            }
            Expr::Attribute(a) => {
                // attribute names count as references (obj.set_x(v) uses set_x)
                self.refs.insert(a.attr.to_string());
            }
            Expr::StringLiteral(s) if !self.is_test => {
                self.strings.push(s.value.to_str().to_string());
            }
            _ => {}
        }
        // Calls are walked manually so keyword values get a Keyword parent —
        // the generic walk would hand them the Call as parent, and the magic
        // position rule must match Python's (status_code=403 is NOT a finding).
        if let Expr::Call(call) = expr {
            let source = self.source;
            boolean_arg_findings(self, &call.arguments.args, source);
            positional_literals_findings(self, call, source);
            // debug-artifact (Python half): breakpoint() left in production —
            // the dbg!/unwrap analog (Rust half lives in rustscan.rs)
            if !self.is_test
                && matches!(
                    call.func.as_ref(),
                    Expr::Name(n) if n.id.as_str() == "breakpoint"
                )
            {
                let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
                self.findings.push(Finding {
                    file: self.file.to_string(),
                    line: line_of(source, call.range().start()),
                    function: fn_name,
                    kind: "debug-artifact".into(),
                    severity: "fail".into(),
                    message: "breakpoint() left in production code — remove it".into(),
                });
            }
            self.parent_stack.push(ParentEntry::Expr(expr.clone()));
            self.visit_expr(&call.func);
            for arg in &call.arguments.args {
                self.visit_expr(arg);
            }
            for kw in &call.arguments.keywords {
                self.parent_stack.push(ParentEntry::Keyword);
                self.visit_expr(&kw.value);
                self.parent_stack.pop();
            }
            self.parent_stack.pop();
            return;
        }
        // Class bodies are not walked for magic numbers (matching the
        // pre-radonc behavior; CC itself now lives in the radonc crate).
        if self.in_class > 0 {
            return;
        }
        // magic numbers (a warn finding — distinct from the CC that radonc owns)
        if let Expr::NumberLiteral(n) = expr {
            self.magic_check(n);
        }
        self.parent_stack.push(ParentEntry::Expr(expr.clone()));
        walk_expr(self, expr);
        self.parent_stack.pop();
    }
}

/// Classify a Python function body's shape for the complexity message: a
/// dispatch chain (>= 3 top-level ifs comparing the SAME selector to string
/// literals) or a rule battery (>= 3 top-level ifs appending to the SAME
/// list) get shape-specific lucid guidance; anything else is "plain". The
/// scan sees the body once — the message must not re-parse (review: run_tool
/// is a dispatch chain, _deterministic_violations a rule battery).
fn python_fn_shape(body: &[Stmt]) -> (&'static str, String) {
    // the OFFER must equal the FIX: a shape is only "dispatch"/"rules" when
    // the fix's own shape checks pass — otherwise the directive would offer
    // something that refuses (the fix-engine v1 boundaries are strict)
    if let Some(sel) = dispatch_chain_shape(body) {
        return ("dispatch", sel);
    }
    if let Some(acc) = rule_battery_shape(body) {
        return ("rules", acc);
    }
    ("plain", String::new())
}

/// One dispatch arm's selector when the test is `sel == "lit"`.
fn dispatch_arm_selector(i: &StmtIf) -> Option<(&str, &str)> {
    let Expr::Compare(c) = i.test.as_ref() else { return None };
    if c.ops.len() != 1 || !matches!(c.ops[0], ruff_python_ast::CmpOp::Eq) {
        return None;
    }
    let Expr::Name(n) = c.left.as_ref() else { return None };
    let Expr::StringLiteral(_) = &c.comparators[0] else {
        return None;
    };
    Some((n.id.as_str(), "lit"))
}

/// The names BOUND (assigned) in a statement run — arm-local bindings.
/// Mirrors fix_engine's _BoundNames (Assign/AnnAssign/For/With targets and
/// walrus/import bindings): a name bound in one dispatch arm and READ in a
/// sibling cannot be a uniform handler parameter.
struct DispatchArmNames<'a> {
    bound: HashSet<&'a str>,
    read: HashSet<&'a str>,
}

fn collect_dispatch_arm_names(body: &[Stmt]) -> DispatchArmNames<'_> {
    use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
    struct Collect<'a> {
        bound: HashSet<&'a str>,
        read: HashSet<&'a str>,
    }
    impl<'a> SourceOrderVisitor<'a> for Collect<'a> {
        fn visit_expr(&mut self, e: &'a Expr) {
            match e {
                Expr::Name(n) => {
                    self.read.insert(n.id.as_str());
                }
                Expr::Named(n) => {
                    if let Expr::Name(t) = n.target.as_ref() {
                        self.bound.insert(t.id.as_str());
                    }
                }
                _ => {}
            }
            ruff_python_ast::visitor::source_order::walk_expr(self, e);
        }
        fn visit_stmt(&mut self, s: &'a Stmt) {
            match s {
                Stmt::Assign(a) => {
                    for t in &a.targets {
                        if let Expr::Name(t) = t {
                            self.bound.insert(t.id.as_str());
                        }
                    }
                }
                Stmt::AnnAssign(a) => {
                    if let Expr::Name(n) = a.target.as_ref() {
                        self.bound.insert(n.id.as_str());
                    }
                }
                Stmt::AugAssign(a) => {
                    if let Expr::Name(n) = a.target.as_ref() {
                        self.bound.insert(n.id.as_str());
                    }
                }
                Stmt::For(f) => {
                    if let Expr::Name(n) = f.target.as_ref() {
                        self.bound.insert(n.id.as_str());
                    }
                }
                Stmt::With(w) => {
                    for item in &w.items {
                        if let Some(v) = &item.optional_vars {
                            if let Expr::Name(n) = v.as_ref() {
                                self.bound.insert(n.id.as_str());
                            }
                        }
                    }
                }
                Stmt::Import(imp) => {
                    for a in &imp.names {
                        self.bound.insert(a.name.as_str());
                    }
                }
                Stmt::ImportFrom(imp) => {
                    for a in &imp.names {
                        self.bound.insert(a.name.as_str());
                    }
                }
                _ => {}
            }
            // comprehension targets bind arm-locally (mirrors fix_engine's
            // visit_CompFor)
            for gen in stmt_comprehension_generators(s) {
                if let Expr::Name(n) = &gen.target {
                    self.bound.insert(n.id.as_str());
                }
            }
            walk_stmt(self, s);
        }
    }
    let mut c = Collect {
        bound: HashSet::new(),
        read: HashSet::new(),
    };
    for s in body {
        walk_stmt(&mut c, s);
    }
    DispatchArmNames {
        bound: c.bound,
        read: c.read,
    }
}

/// The comprehension generators of a statement's expressions — their targets
/// bind arm-locally (mirrors fix_engine's visit_CompFor).
fn stmt_comprehension_generators(s: &Stmt) -> Vec<&ruff_python_ast::Comprehension> {
    fn collect_expr<'a>(e: &'a Expr, out: &mut Vec<&'a ruff_python_ast::Comprehension>) {
        match e {
            Expr::ListComp(c) => {
                out.extend(c.generators.iter());
                collect_expr(&c.elt, out);
            }
            Expr::SetComp(c) => {
                out.extend(c.generators.iter());
                collect_expr(&c.elt, out);
            }
            Expr::DictComp(c) => {
                out.extend(c.generators.iter());
                if let Some(k) = &c.key {
                    collect_expr(k, out);
                }
                collect_expr(&c.value, out);
            }
            Expr::Generator(g) => {
                out.extend(g.generators.iter());
                collect_expr(&g.elt, out);
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    match s {
        Stmt::Assign(a) => collect_expr(a.value.as_ref(), &mut out),
        Stmt::AnnAssign(a) => {
            if let Some(v) = &a.value {
                collect_expr(v.as_ref(), &mut out);
            }
        }
        Stmt::Expr(e) => collect_expr(e.value.as_ref(), &mut out),
        Stmt::Return(r) => {
            if let Some(v) = &r.value {
                collect_expr(v.as_ref(), &mut out);
            }
        }
        Stmt::If(i) => {
            for b in &i.body {
                out.extend(stmt_comprehension_generators(b));
            }
        }
        _ => {}
    }
    out
}

/// The FIXABLE dispatch shape: a preamble, >= 3 contiguous `if <sel> ==
/// "lit": <body>` arms over ONE selector, then exactly one trailing
/// `return <default>`. This mirrors fix_engine's _dispatch_chain_shape — the
/// offer is only made when the fix applies. Arms whose body reads a name
/// BOUND IN A SIBLING ARM are refused (the named-handler rewrite cannot
/// preserve them — see _dispatch_named_mode).
fn dispatch_chain_shape(body: &[Stmt]) -> Option<String> {
    let first_if = body.iter().position(|s| matches!(s, Stmt::If(_)))?;
    let mut selector: Option<&str> = None;
    let mut n = 0usize;
    for stmt in &body[first_if..] {
        let Stmt::If(i) = stmt else { break };
        let (sel, _lit) = dispatch_arm_selector(i)?;
        if !i.elif_else_clauses.is_empty() {
            return None; // an arm with elif/else is not the fixable shape
        }
        match selector {
            None => selector = Some(sel),
            Some(s) if s == sel => {}
            _ => return None,
        }
        n += 1;
    }
    if n < 3 {
        return None;
    }
    // the no-match path: a single trailing `return <default>`
    let tail = &body[first_if + n..];
    if tail.len() != 1 || !matches!(tail[0], Stmt::Return(_)) {
        return None;
    }
    // sibling-bound reads: an arm reading a name another arm binds cannot be
    // rewritten to uniform module-level handlers (the value does not exist
    // at the call site) — parity with the fix's refusal
    let arms: Vec<&[Stmt]> = body[first_if..first_if + n]
        .iter()
        .filter_map(|s| match s {
            Stmt::If(i) => Some(i.body.as_slice()),
            _ => None,
        })
        .collect();
    let names: Vec<DispatchArmNames> = arms.iter().map(|b| collect_dispatch_arm_names(b)).collect();
    for (i, arm) in names.iter().enumerate() {
        for (j, other) in names.iter().enumerate() {
            if i != j && arm.read.intersection(&other.bound).next().is_some() {
                return None;
            }
        }
    }
    selector.map(str::to_string)
}

/// The `acc = []` opener (plain or annotated) — (acc name, index).
fn empty_list_init(body: &[Stmt]) -> Option<(String, usize)> {
    for (i, stmt) in body.iter().enumerate() {
        match stmt {
            Stmt::Assign(a) => {
                if matches!(a.value.as_ref(), Expr::List(l) if l.elts.is_empty())
                    && a.targets.len() == 1
                    && matches!(a.targets[0], Expr::Name(_))
                {
                    let Expr::Name(n) = &a.targets[0] else { unreachable!() };
                    return Some((n.id.to_string(), i));
                }
            }
            Stmt::AnnAssign(a) => match (a.value.as_deref(), a.target.as_ref()) {
                (Some(Expr::List(l)), Expr::Name(n)) if l.elts.is_empty() => {
                    return Some((n.id.to_string(), i));
                }
                _ => {}
            },
            _ => {}
        }
    }
    None
}

/// Is the statement exactly one `acc.append(<value>)` call?
fn is_single_append(stmts: &[Stmt], acc: &str) -> bool {
    if stmts.len() != 1 {
        return false;
    }
    let Stmt::Expr(e) = &stmts[0] else { return false };
    let Expr::Call(call) = e.value.as_ref() else {
        return false;
    };
    let Expr::Attribute(a) = call.func.as_ref() else {
        return false;
    };
    a.attr.as_str() == "append"
        && matches!(a.value.as_ref(), Expr::Name(n) if n.id.as_str() == acc)
        && call.arguments.args.len() == 1
}

/// The FIXABLE rule-battery shape: an `acc = []` opener, then >= 3
/// CONTIGUOUS `if <cond>: acc.append(<v>)` checks (no else, one append
/// each). Mirrors fix_engine's _rule_battery_shape — the offer equals the
/// fix.
fn rule_battery_shape(body: &[Stmt]) -> Option<String> {
    let (acc, init_idx) = empty_list_init(body)?;
    let mut n = 0usize;
    for stmt in &body[init_idx + 1..] {
        let Stmt::If(i) = stmt else { break };
        if !i.elif_else_clauses.is_empty() || !is_single_append(&i.body, &acc) {
            return None;
        }
        n += 1;
    }
    if n < 3 {
        return None;
    }
    Some(acc)
}

impl<'a> ScanState<'a> {
    /// Magic numbers: int/float literals outside (0, 1, 2) whose parent is an
    /// operation — mirrors the Python implementation's position rule.
    fn magic_check(&mut self, n: &ruff_python_ast::ExprNumberLiteral) {
        let value = match &n.value {
            ruff_python_ast::Number::Int(i) => i.to_string(),
            ruff_python_ast::Number::Float(f) => f.to_string(),
            ruff_python_ast::Number::Complex { .. } => return,
        };
        if matches!(value.as_str(), "0" | "1" | "2") {
            return;
        }
        let parent_is_op = matches!(
            self.parent_stack.last(),
            Some(ParentEntry::Expr(
                Expr::BinOp(_) | Expr::Compare(_) | Expr::UnaryOp(_) | Expr::Subscript(_) | Expr::Call(_)
            ))
        );
        if !parent_is_op {
            return;
        }
        let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        self.findings.push(Finding {
            file: self.file.to_string(),
            line: line_of(self.source, n.range.start()),
            function: fn_name,
            kind: "magic-number".into(),
            severity: "warn".into(),
            message: format!("magic number {value} — name it as a constant — fix: magic-number --fix-name <CONST>"),
        });
    }

    /// No-op statements: expression statements that discard their value.
    fn noop_check(&mut self, v: &'a Expr) {
        let harmless = matches!(
            v,
            Expr::Call(_)
                | Expr::Await(_)
                | Expr::Yield(_)
                | Expr::YieldFrom(_)
                | Expr::StringLiteral(_)
                | Expr::BytesLiteral(_)
                | Expr::NumberLiteral(_)
                | Expr::BooleanLiteral(_)
                | Expr::NoneLiteral(_)
                | Expr::EllipsisLiteral(_)
                | Expr::Lambda(_)
                | Expr::Named(_)
        );
        if !harmless {
            let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
            self.findings.push(Finding {
                file: self.file.to_string(),
                line: line_of(self.source, v.range().start()),
                function: fn_name,
                kind: "noop-statement".into(),
                severity: "fail".into(),
                message: "expression statement discards its value — dead statement — fix: noop-statement".into(),
            });
        }
    }

    /// Inline imports (imports inside any function) and private-symbol
    /// imports (`from pkg import _x`, `from pkg._sub import x`,
    /// `import pkg._internal`).
    fn import_checks(&mut self, stmt: &'a Stmt) {
        let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        if !self.fn_stack.is_empty() {
            let line = stmt_line(self.source, stmt);
            self.findings.push(Finding {
                file: self.file.to_string(),
                line,
                function: fn_name.clone(),
                kind: "inline-import".into(),
                severity: "fail".into(),
                message: format!("import inside function body at line {line} — move it to module top"),
            });
        }
        match stmt {
            Stmt::ImportFrom(im) => {
                if im.module.as_ref().map(|m| m.as_str()) == Some("__future__") {
                    return;
                }
                let private_module = im
                    .module
                    .as_ref()
                    .map(|m| m.as_str().split('.').any(|seg| seg.starts_with('_')))
                    .unwrap_or(false);
                for alias in &im.names {
                    let name = alias.name.as_str();
                    if name.starts_with('_') || private_module {
                        let target = if name.starts_with('_') {
                            name.to_string()
                        } else {
                            format!("{}.{}", im.module.as_ref().map(|m| m.as_str()).unwrap_or(""), name)
                        };
                        self.findings.push(Finding {
                            file: self.file.to_string(),
                            line: stmt_line(self.source, stmt),
                            function: fn_name.clone(),
                            kind: "private-import".into(),
                            severity: "fail".into(),
                            message: format!("imports private symbol '{target}' — never import underscore symbols"),
                        });
                    }
                }
            }
            Stmt::Import(im) => {
                for alias in &im.names {
                    let name = alias.name.as_str();
                    if name.split('.').any(|seg| seg.starts_with('_')) {
                        self.findings.push(Finding {
                            file: self.file.to_string(),
                            line: stmt_line(self.source, stmt),
                            function: fn_name.clone(),
                            kind: "private-import".into(),
                            severity: "fail".into(),
                            message: format!("imports private path '{name}' — never import underscore symbols"),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    /// Statements after an unconditional return/raise/continue/break are dead
    /// code — one finding per statement list, exactly like the Python tool.
    fn unreachable_check(&mut self, body: &'a [Stmt]) {
        let mut lists: Vec<&'a [Stmt]> = vec![body];
        for s in body {
            collect_stmt_lists(s, &mut lists);
        }
        let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        for list in lists {
            for (i, stmt) in list.iter().enumerate() {
                let exits = matches!(
                    stmt,
                    Stmt::Return(_) | Stmt::Raise(_) | Stmt::Continue(_) | Stmt::Break(_)
                );
                if exits {
                    if let Some(dead) = list.get(i + 1) {
                        let line = stmt_line(self.source, dead);
                        self.findings.push(Finding {
                            file: self.file.to_string(),
                            line,
                            function: fn_name.clone(),
                            kind: "unreachable".into(),
                            severity: "fail".into(),
                            message: format!(
                                "unreachable statement at line {line} — dead code is deleted — fix: unreachable"
                            ),
                        });
                    }
                    break;
                }
            }
        }
    }
}

/// All statement lists inside a function, excluding nested function bodies —
/// the Python `_statement_lists` equivalent.
fn collect_stmt_lists<'a>(stmt: &'a Stmt, out: &mut Vec<&'a [Stmt]>) {
    match stmt {
        Stmt::FunctionDef(_) => {}
        Stmt::ClassDef(c) => {
            out.push(&c.body);
            for s in &c.body {
                collect_stmt_lists(s, out);
            }
        }
        Stmt::If(i) => {
            out.push(&i.body);
            for s in &i.body {
                collect_stmt_lists(s, out);
            }
            for cl in &i.elif_else_clauses {
                out.push(&cl.body);
                for s in &cl.body {
                    collect_stmt_lists(s, out);
                }
            }
        }
        Stmt::For(f) => {
            out.push(&f.body);
            for s in &f.body {
                collect_stmt_lists(s, out);
            }
            out.push(&f.orelse);
            for s in &f.orelse {
                collect_stmt_lists(s, out);
            }
        }
        Stmt::While(w) => {
            out.push(&w.body);
            for s in &w.body {
                collect_stmt_lists(s, out);
            }
            out.push(&w.orelse);
            for s in &w.orelse {
                collect_stmt_lists(s, out);
            }
        }
        Stmt::With(w) => {
            out.push(&w.body);
            for s in &w.body {
                collect_stmt_lists(s, out);
            }
        }
        Stmt::Try(t) => {
            out.push(&t.body);
            for s in &t.body {
                collect_stmt_lists(s, out);
            }
            for h in &t.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                out.push(&eh.body);
                for s in &eh.body {
                    collect_stmt_lists(s, out);
                }
            }
            out.push(&t.orelse);
            for s in &t.orelse {
                collect_stmt_lists(s, out);
            }
            out.push(&t.finalbody);
            for s in &t.finalbody {
                collect_stmt_lists(s, out);
            }
        }
        Stmt::Match(m) => {
            for case in &m.cases {
                out.push(&case.body);
                for s in &case.body {
                    collect_stmt_lists(s, out);
                }
            }
        }
        _ => {}
    }
}

fn stmt_line(source: &str, stmt: &Stmt) -> usize {
    let off = match stmt {
        Stmt::FunctionDef(f) => f.range.start(),
        Stmt::ClassDef(c) => c.range.start(),
        Stmt::Return(r) => r.range.start(),
        Stmt::Delete(d) => d.range.start(),
        Stmt::TypeAlias(t) => t.range.start(),
        Stmt::Assign(a) => a.range.start(),
        Stmt::AugAssign(a) => a.range.start(),
        Stmt::AnnAssign(a) => a.range.start(),
        Stmt::For(f) => f.range.start(),
        Stmt::While(w) => w.range.start(),
        Stmt::If(i) => i.range.start(),
        Stmt::With(w) => w.range.start(),
        Stmt::Match(m) => m.range.start(),
        Stmt::Raise(r) => r.range.start(),
        Stmt::Try(t) => t.range.start(),
        Stmt::Assert(a) => a.range.start(),
        Stmt::Import(i) => i.range.start(),
        Stmt::ImportFrom(i) => i.range.start(),
        Stmt::Global(g) => g.range.start(),
        Stmt::Nonlocal(n) => n.range.start(),
        Stmt::Expr(e) => e.range.start(),
        Stmt::Pass(p) => p.range.start(),
        Stmt::Break(b) => b.range.start(),
        Stmt::Continue(c) => c.range.start(),
        Stmt::IpyEscapeCommand(s) => s.range.start(),
    };
    line_of(source, off)
}

/// One file's scan output, including the repo-wide collections.
/// One top-level class: name, def line, abstract flag, and base references
/// ("Name:Id" for a plain base, "Attr:Alias:attr" for alias.attr).
#[derive(Clone)]
pub struct ClassInfo {
    pub name: String,
    pub line: usize,
    pub abstract_: bool,
    pub bases: Vec<String>,
}

/// One import alias: local name -> (module, imported name).
#[derive(Clone)]
pub struct ImportInfo {
    pub alias: String,
    pub module: String,
    pub imported: String,
}

pub struct FileScan {
    pub file_name: String,
    pub findings: Vec<Finding>,
    pub cc: Vec<FnCc>,
    pub errors: usize,
    pub defs: Vec<(String, usize)>,
    pub refs: HashSet<String>,
    pub strings: Vec<String>,
    pub decorated: HashSet<String>,
    pub skeletons: Vec<SkeletonFn>,
    pub classes: Vec<ClassInfo>,
    pub imports: Vec<ImportInfo>,
    /// The file's parsed `lucidlint: ignore` suppressions — the repo-wide
    /// families (duplicate/unused/docs/graph) are filtered through them at
    /// the end of the scan (per-file families apply theirs during the scan).
    pub supps: common::Suppressions,
}

pub(crate) fn is_test_path(name: &str) -> bool {
    name.contains("/test") || name.starts_with("test")
}

/// The full scan (CLI): per-file families + repo-wide collections.
fn scan_source(source: &str, name: &str) -> FileScan {
    let repo_wide = true;
    scan_source_impl(source, name, repo_wide)
}

/// The LSP scan: per-file families only — the duplicate-candidate BFS and
/// its per-function skeleton walks are pure waste for a single buffer.
/// Dispatches on the buffer's extension: .rs -> the Rust layer, else Python.
pub fn scan_source_lsp(source: &str, name: &str) -> FileScan {
    if name.ends_with(".rs") {
        let repo_wide = false; // LSP per-buffer scans are single-file
        let rs = rustscan::scan_source(source, name, repo_wide);
        return rustscan_to_filescan_ref(&rs, name);
    }
    let repo_wide = false; // LSP per-buffer scans are single-file
    scan_source_impl(source, name, repo_wide)
}

/// The module-level post-passes — structural families + the advisory
/// refactorings — kept out of scan_source_impl (which was crossing the
/// large-function line).
fn module_post_passes(state: &mut ScanState, body: &[Stmt], name: &str, source: &str) {
    class_module_findings(state, body, name);
    vague_name_findings(state, body);
    strewing_findings(state, body);
    record_shape_findings(state, body, source);
    partition_findings(state, body, source);
    // review-log rules: shadowing hazards + duplicated work (all per-file)
    duplicate_def_findings(state, body);
    restating_docstring_findings(state, body);
    duplicate_block_findings(state, body);
    // advisory refactorings (warn): detection-only, the fix names the Fowler
    // refactoring for the agent (or a future fix)
    guard_clause_findings(state, body, source);
    latent_visitor_findings(state, body, source); // claims chains first — polymorphism skips them
    conditional_polymorphism_findings(state, body, source);
    special_case_findings(state, body, source);
    middle_man_findings(state, body, source);
    unused_setter_findings(state, body, source);
    loop_pipeline_findings(state, body, source);
}

fn scan_source_impl(source: &str, name: &str, repo_wide: bool) -> FileScan {
    let parsed: Parsed<ModModule> = match parse_module(source) {
        Ok(p) => p,
        Err(_) => {
            return FileScan {
                file_name: name.to_string(),
                findings: Vec::new(),
                cc: Vec::new(),
                errors: 1,
                defs: Vec::new(),
                refs: HashSet::new(),
                strings: Vec::new(),
                decorated: HashSet::new(),
                skeletons: Vec::new(),
                classes: Vec::new(),
                imports: Vec::new(),
                supps: common::Suppressions::default(),
            };
        }
    };
    let errors = parsed.errors().len();
    let body = parsed.syntax().body.clone(); // ThinVec<Stmt> — derefs to &[Stmt]
    let mut state = ScanState {
        file: name,
        source,
        module_mutables: module_container_names(&body),
        module_flagged: module_flagged_names(&body),
        is_test: is_test_path(name),
        ..Default::default()
    };
    for stmt in &body {
        state.visit_stmt(stmt);
    }
    module_post_passes(&mut state, &body, name, source);
    if state.is_test {
        // the rules that live in tests — scanned for alone or with --include-tests
        monkeypatch_findings(&mut state, &body, source);
        skipif_findings(&mut state, &body, source);
        fakefs_findings(&mut state, &body, source);
        no_assert_test_findings(&mut state, &body, source);
    }
    // duplicate candidates in ast.walk BFS order — module-level functions
    // (any depth) come before class methods regardless of source line;
    // skipped for a single buffer (repo-wide families need the whole repo)
    if repo_wide && !state.is_test {
        let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
        let mut qi = 0usize;
        while qi < queue.len() {
            if let Q::N(n) = queue[qi] {
                if let AnyNodeRef::StmtFunctionDef(f) = n {
                    let skel = fn_skeleton(f);
                    if is_duplicate_candidate(f, skel.len()) {
                        let def_line = line_of(source, f.name.range().start());
                        state.skeletons.push(SkeletonFn {
                            rel: name.to_string(),
                            name: f.name.to_string(),
                            line: def_line,
                            skeleton: skel,
                        });
                    }
                }
                skel_children(n, &mut queue);
            }
            qi += 1;
        }
    }
    let tokens = parsed.tokens();
    let supps = checks::parse_suppressions(source, tokens);
    // complexity findings are generated from the cc array after this pass —
    // honor complexity suppressions here so both paths agree (radon parity:
    // a fn whose complexity is suppressed must not resurface via cc)
    let mut pre_used = common::PreUsedSuppressions::default();
    if !state.cc.is_empty() {
        state.cc.retain(|e| {
            // record what the retention's own decision honors — the SAME
            // widened window and family-aware matching that suppressed()
            // uses (a suppression used by the cc path must not be re-flagged
            // stale by apply_suppressions_impl)
            for ln in common::window_lines(e.line) {
                if let Some(entries) = supps.line.get(&ln) {
                    for (sig, why) in entries {
                        if common::signal_matches(sig, "complexity") && !why.is_empty() {
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
    let findings = checks::apply_suppressions_impl(state.findings, source, name, tokens, &pre_used);
    let mut all = type_ignore_findings(source, name, tokens);
    all.extend(noqa_findings(source, name, tokens));
    all.extend(findings);
    let classes = collect_classes(&body, source);
    let imports = collect_imports(&body, source);
    FileScan {
        file_name: name.to_string(),
        findings: all,
        cc: state.cc,
        errors,
        defs: state.defs,
        refs: state.refs,
        strings: state.strings,
        decorated: state.decorated,
        skeletons: state.skeletons,
        classes,
        imports,
        supps,
    }
}

pub(crate) fn scan_file(path: &Path) -> FileScan {
    let source = std::fs::read_to_string(path).unwrap_or_default();
    let name = path.to_str().unwrap_or("<file>").to_string();
    if name.ends_with(".rs") {
        let repo_wide = true;
        let rs = rustscan::scan_source(&source, &name, repo_wide);
        return rustscan_to_filescan_ref(&rs, &name);
    }
    let mut scan = scan_source(&source, &name);
    scan.file_name = name;
    scan
}

/// Directories a repo-wide scan never enters (venvs, caches, build output —
/// the orchestrator's git-ls-files equivalent without git).
const SCAN_SKIP_DIRS: &[&str] = &[
    ".git",
    ".venv",
    "venv",
    "node_modules",
    "__pycache__",
    ".lucidlint-cache",
    ".ruff_cache",
    ".pytest_cache",
    ".mypy_cache",
    ".pyrefly-cache",
    "htmlcov",
    "dist",
    "build",
    "target",
    ".code-review-graph",
    ".tox",
    ".eggs",
];

/// The repo-wide merge for the LSP's save path: every Python file under
/// `root` scanned with the repo-wide flag, then the families that need the
/// whole repo (duplicate, unused — reconciled through each file's
/// suppressions, review-log B3). Returns the ADDITIONS per repo-relative
/// file; the per-file findings travel via the buffer scan so nothing is
/// double-reported. Lives here (the composition root) so lsp stays a
/// standalone scan-core module (layers test).
pub(crate) fn repo_wide_scan(root: &Path) -> std::collections::HashMap<String, Vec<Finding>> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !SCAN_SKIP_DIRS.contains(&name) {
                    stack.push(p);
                }
            } else if p.extension().is_some_and(|x| x == "py") {
                paths.push(p);
            }
        }
    }
    paths.sort();
    let scans: Vec<FileScan> = paths.par_iter().map(|p| scan_file(p)).collect();
    let root_s = root.to_string_lossy().to_string();
    let mut skeletons = Vec::new();
    let mut definitions: Vec<(String, String, usize)> = Vec::new();
    let mut prod_refs = HashSet::new();
    let mut test_refs = HashSet::new();
    let mut strings = Vec::new();
    let mut supps_by_rel: std::collections::HashMap<String, common::Suppressions> = std::collections::HashMap::new();
    for scan in &scans {
        let rel = rel_of(&scan.file_name, &root_s);
        let is_test = is_test_path(&rel);
        for s in &scan.skeletons {
            skeletons.push(SkeletonFn {
                rel: rel.clone(),
                name: s.name.clone(),
                line: s.line,
                skeleton: s.skeleton.clone(),
            });
        }
        for (name, line) in &scan.defs {
            definitions.push((rel.clone(), name.clone(), *line));
        }
        if is_test {
            test_refs.extend(scan.refs.iter().cloned());
        } else {
            prod_refs.extend(scan.refs.iter().cloned());
            strings.extend(scan.strings.iter().cloned());
        }
        prod_refs.extend(scan.decorated.iter().cloned());
        supps_by_rel.insert(rel.clone(), scan.supps.clone());
    }
    let mut additions: Vec<Finding> = Vec::new();
    additions.extend(checks::duplicate_findings(&skeletons));
    let repo_wide_unused = checks::unused_findings(&definitions, &prod_refs, &test_refs, &strings);
    reconcile_repo_wide(&mut additions, repo_wide_unused, &supps_by_rel);
    let mut by_file: std::collections::HashMap<String, Vec<Finding>> = std::collections::HashMap::new();
    for f in additions {
        by_file.entry(f.file.clone()).or_default().push(f);
    }
    by_file
}

/// The Rust layer's scan into the shared FileScan shape. The Python-specific
/// collections (defs/refs/strings/decorated/classes/imports) stay empty:
/// `unused` is rustc dead_code's job and the graph families need the Rust
/// exporter — the rustscan data (mod/use/structs) travels via `RustScan`.
fn rustscan_to_filescan_ref(rs: &rustscan::RustScan, name: &str) -> FileScan {
    let supps = rs.supps.clone();
    FileScan {
        file_name: name.to_string(),
        findings: rs.findings.clone(),
        cc: rs.cc.clone(),
        errors: rs.errors,
        defs: Vec::new(),
        refs: HashSet::new(),
        strings: Vec::new(),
        decorated: HashSet::new(),
        skeletons: rs.skeletons.clone(),
        classes: Vec::new(),
        imports: Vec::new(),
        supps,
    }
}

///
/// The finding model's final action kind — the JSON contract carries it so
/// the Python orchestrator consumes findings without further mapping.
///
/// Two identifiers per finding, don't conflate them:
///
/// - `kind` — display kind, `final_kind` output (named buckets; families
///   without one collapse to "standard", the message explains the rest).
/// - `signal` — the raw family kind; THIS is what suppressions match on
///   (`lucidlint: ignore <signal>`, config `ignore = [<signal>]`,
///   RULE_GROUPS membership, baseline identity).
///
/// FAMILY_KINDS, STANDARD_KINDS, `final_kind`, and `rule_groups` are
/// GENERATED from the rule catalog (rule_metadata.py) by `make rules` — see
/// rules_gen.rs. Registering a rule is ONE edit in the catalog; everything
/// here follows from it and the drift gate pins the generated file.
pub use rules_gen::{final_kind, FAMILY_KINDS, STANDARD_KINDS};

/// Longest common prefix of the passed paths — the repo root for a
/// full-repo run; the file itself in per-file mode.
///
/// The char-level prefix can stop mid-component (tests/ vs tools/ share a
/// trailing 't'), so it backs off to the last path separator — rels are
/// always clean directory boundaries.
fn repo_root(paths: &[String]) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    repo_root_impl(paths, &cwd)
}

/// The longest common prefix of the passed paths, backed off to the last
/// path separator. Paths are resolved against `cwd` FIRST: the orchestrator
/// passes REPO-RELATIVE paths (the binary runs with the repo as its cwd),
/// and the raw common prefix of `tests/a.py` + `tools/b.py` is `"t"` — which
/// backed off to `""` and silently broke every graph-family rel resolution
/// (repo_rel on the graph's ABSOLUTE node paths stripped nothing), so
/// large-function/hub-file/high-risk/layer-mix/folder-mix never fired
/// through the orchestrator. Joining the cwd makes the root the absolute
/// repo path for relative inputs and leaves absolute inputs unchanged.
fn repo_root_impl(paths: &[String], cwd: &Path) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let abs: Vec<String> = paths
        .iter()
        .map(|p| {
            let pb = Path::new(p);
            if pb.is_absolute() {
                p.clone()
            } else {
                cwd.join(pb).to_string_lossy().to_string()
            }
        })
        .collect();
    let mut prefix = abs[0].clone();
    for p in &abs[1..] {
        while !p.starts_with(&prefix) {
            prefix.truncate(prefix.len().saturating_sub(1));
        }
    }
    match prefix.rfind('/') {
        Some(idx) => prefix.truncate(idx),
        None => prefix.clear(),
    }
    prefix
}
fn sig_of_stale(msg: &str) -> String {
    // line form: "suppression '// lucidlint: ignore <sig>' at line N no longer fires"
    if let Some(i) = msg.find("ignore ") {
        return msg[i + 7..]
            .split(|c: char| c.is_whitespace() || c == '\'' || c == '>')
            .next()
            .unwrap_or("")
            .to_string();
    }
    // file form: "file suppression '... ignore-file <sig>' no longer fires"
    if let Some(i) = msg.find("ignore-file ") {
        return msg[i + 12..]
            .split(|c: char| c.is_whitespace() || c == '\'')
            .next()
            .unwrap_or("")
            .to_string();
    }
    String::new()
}

/// Repo-wide findings (unused, duplicate) are computed after the per-file
/// suppression pass, so an `# lucidlint: ignore <sig>` comment for one was
/// never consumed and got flagged stale — and the finding stayed. Re-honor
/// the file's suppressions for those repo-wide findings (family-aware, widened
/// window) and drop the stale-suppression findings the consumed comments
/// caused (review-log B3). The survivors are appended to `all` in place.
// lucidlint: ignore record-shape Finding is the core finding type consumed repo-wide — one more consumer is not a new record
pub(crate) fn reconcile_repo_wide(
    all: &mut Vec<Finding>,
    repo_wide: Vec<Finding>,
    supps_by_rel: &std::collections::HashMap<String, common::Suppressions>,
) {
    let mut used_line: std::collections::HashSet<(usize, String)> = std::collections::HashSet::new();
    let mut used_file: std::collections::HashSet<String> = std::collections::HashSet::new();
    for f in repo_wide {
        match supps_by_rel.get(&f.file) {
            Some(supps) => {
                let kept = common::filter_repo_wide(vec![f], supps, &mut used_line, &mut used_file);
                all.extend(kept);
            }
            None => all.push(f),
        }
    }
    if used_line.is_empty() && used_file.is_empty() {
        return;
    }
    all.retain(|g| {
        if g.kind != "stale-suppression" {
            return true;
        }
        let sig = sig_of_stale(&g.message);
        !used_file.contains(&sig) && !used_line.contains(&(g.line, sig))
    });
}

pub(crate) fn rel_of(path: &str, root: &str) -> String {
    // a relative path is resolved against the cwd (the binary runs with the
    // repo as its cwd) — stripping the root then yields the true rel instead
    // of falling back to the bare file name (which left per-file rels to be
    // re-resolved by the orchestrator's cwd-dependent guess)
    let abs = if Path::new(path).is_absolute() {
        path.to_string()
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(path)
            .to_string_lossy()
            .to_string()
    };
    if let Some(rel) = abs.strip_prefix(root).and_then(|r| r.strip_prefix('/')) {
        return rel.to_string();
    }
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

// lucidlint: ignore large-function the CLI orchestrates every repo-wide family in one flow — extracting helpers would thread six collections
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        // the release pipeline stamps CODE_HEALTH_VERSION at build time; the
        // crate version is the local/dev fallback
        let version = option_env!("CODE_HEALTH_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
        println!("lucidlint {version}");
        return;
    }
    if args.first().map(String::as_str) == Some("--lsp") {
        // stdio JSON-RPC language server — in-process scans, no spawns
        lsp::run();
        return;
    }
    if args.first().map(String::as_str) == Some("--fix") {
        // the Rust fix surface the orchestrator dispatches to for `.rs`
        // targets: `--fix '{"kind":...,"file":...,"line":N,"name":...}'`.
        // Reads the file, applies the fix IN PLACE, prints a status line.
        if let Some(spec) = args.get(1) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(spec) {
                let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                let file = v.get("file").and_then(|k| k.as_str()).unwrap_or("");
                let line = v.get("line").and_then(|k| k.as_u64()).unwrap_or(0) as usize;
                let name = v.get("name").and_then(|k| k.as_str()).unwrap_or("");
                if kind == "extract-method" || kind == "dispatch-registry" || kind == "rule-table" {
                    let src = match std::fs::read_to_string(file) {
                        Ok(s) => s,
                        Err(e) => {
                            // R29: an unexpected error surfaces the ACTUAL
                            // error — a missing/unreadable file is NOT
                            // "nothing to change" (which would fabricate a
                            // silent success for a finding that may exist)
                            eprintln!("fix: cannot read {file}: {e}");
                            std::process::exit(1);
                        }
                    };
                    let result = if kind == "dispatch-registry" {
                        fix::fix_dispatch_registry(&src, line)
                    } else if kind == "rule-table" {
                        fix::fix_rule_table(&src, line)
                    } else {
                        fix::fix_extract_method(&src, line, name)
                    };
                    match result {
                        Ok(out) => {
                            if std::fs::write(file, out).is_err() {
                                println!("fix: could not write the fixed file — {file}:{line}");
                                return;
                            }
                            let what = if kind == "dispatch-registry" {
                                "converted the dispatch chain into a match"
                            } else if kind == "rule-table" {
                                "hoisted the if/append checks into a (condition, violation) table"
                            } else {
                                "extracted seam into a named function"
                            };
                            println!("fix: {what} — {file}:{line} ({kind})");
                            return;
                        }
                        Err(_) => {
                            // R28: never explain an absent fix — the
                            // silence is the signal
                            println!("fix: nothing to change for {kind} at {file}:{line}");
                            return;
                        }
                    }
                }
            }
        }
        println!("fix: unknown or malformed --fix request");
        std::process::exit(2);
    }
    // flag parsing: --graph <json> --churn <json> --docs <root> --include-tests
    let mut graph_path: Option<String> = None;
    let mut churn_path: Option<String> = None;
    let mut docs_root: Option<String> = None;
    let mut gitignored_json: Option<String> = None;
    let mut include_tests = false;
    let mut i = 0usize;
    let mut paths: Vec<String> = Vec::new();
    while i < args.len() {
        match args[i].as_str() {
            "--graph" => {
                i += 1;
                graph_path = args.get(i).cloned();
            }
            "--churn" => {
                i += 1;
                churn_path = args.get(i).cloned();
            }
            "--docs" => {
                i += 1;
                docs_root = args.get(i).cloned();
            }
            "--gitignored" => {
                i += 1;
                gitignored_json = args.get(i).cloned();
            }
            "--include-tests" => include_tests = true,
            _ => paths.push(args[i].clone()),
        }
        i += 1;
    }
    let root = repo_root(&paths);
    let mut scans = Vec::new();
    // per-file scans are independent — parse in parallel (rayon), then the
    // repo-wide passes consume the scans in ORDER (duplicate first-seen,
    // unused refs, cycles all depend on the deterministic file order)
    let paired: Vec<(Option<rustscan::RustScan>, FileScan)> = paths
        .par_iter()
        .map(|path| {
            if path.ends_with(".rs") {
                let source = std::fs::read_to_string(path).unwrap_or_default();
                let repo_wide = true;
                let rs = rustscan::scan_source(&source, path, repo_wide);
                let rel = path.clone();
                let fs = rustscan_to_filescan_ref(&rs, &rel);
                (Some(rs), fs)
            } else {
                (None, scan_file(Path::new(path)))
            }
        })
        .collect();
    let mut rust_scans: Vec<rustscan::RustScan> = Vec::new();
    for (rs, fs) in paired {
        if let Some(r) = rs {
            rust_scans.push(r);
        }
        scans.push(fs);
    }
    // repo-wide families: duplicate (Dice, per-language pools) + unused
    // (Python reference scan; Rust's dead code is rustc's, so Rust scans
    // carry no defs/refs and never fire the unused family)
    let mut skeletons = Vec::new();
    let mut definitions: Vec<(String, String, usize)> = Vec::new();
    let mut prod_refs = HashSet::new();
    let mut test_refs = HashSet::new();
    let mut strings = Vec::new();
    for scan in &scans {
        let rel = rel_of(&scan.file_name, &root);
        let is_test = is_test_path(&rel);
        for s in &scan.skeletons {
            skeletons.push(SkeletonFn {
                rel: rel.clone(),
                name: s.name.clone(),
                line: s.line,
                skeleton: s.skeleton.clone(),
            });
        }

        for (name, line) in &scan.defs {
            definitions.push((rel.clone(), name.clone(), *line));
        }
        if is_test {
            test_refs.extend(scan.refs.iter().cloned());
        } else {
            prod_refs.extend(scan.refs.iter().cloned());
            strings.extend(scan.strings.iter().cloned());
        }
        prod_refs.extend(scan.decorated.iter().cloned());
    }
    let mut all_findings = Vec::new();
    let mut all_cc = Vec::new();
    let mut total_errors = 0usize;
    // over-abstraction: ABCs with exactly one concrete subclass (needs the
    // class/import collection before the scans are consumed)
    let class_scans: Vec<(String, Vec<ClassInfo>, Vec<ImportInfo>)> = scans
        .iter()
        .map(|s| (rel_of(&s.file_name, &root), s.classes.clone(), s.imports.clone()))
        .collect();
    let supps_by_rel: std::collections::HashMap<String, common::Suppressions> = scans
        .iter()
        .map(|s| (rel_of(&s.file_name, &root), s.supps.clone()))
        .collect();
    for scan in scans {
        all_findings.extend(scan.findings);
        all_cc.extend(scan.cc);
        total_errors += scan.errors;
    }
    // per-language duplicate pools: a Python fn and a Rust fn with the same
    // shape are a PORT, not a copy-paste duplicate — never cross-matched
    let mut py_skeletons: Vec<SkeletonFn> = Vec::new();
    let mut rs_skeletons: Vec<SkeletonFn> = Vec::new();
    for s in skeletons {
        if s.rel.ends_with(".rs") {
            rs_skeletons.push(s);
        } else {
            py_skeletons.push(s);
        }
    }
    all_findings.extend(duplicate_findings(&py_skeletons));
    all_findings.extend(duplicate_findings(&rs_skeletons));
    // unused is a repo-wide family computed AFTER the per-file suppression
    // pass — reconcile it through those suppressions so an `ignore unused <why>`
    // comment suppresses the finding and is not reported stale (review-log B3)
    let repo_wide_unused = unused_findings(&definitions, &prod_refs, &test_refs, &strings);
    reconcile_repo_wide(&mut all_findings, repo_wide_unused, &supps_by_rel);
    // import cycles for Rust crates: the local mod/use graph (the
    // code-review-graph contract is Python-only; Rust resolves itself)
    if !rust_scans.is_empty() {
        let mut mod_decls: std::collections::HashMap<String, Vec<(String, bool)>> = std::collections::HashMap::new();
        let mut uses: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        for rs in &rust_scans {
            let rel = rel_of(&rs.file_name(), &root);
            mod_decls.insert(rel.clone(), rs.mod_decls.clone());
            uses.insert(rel, rs.uses.clone());
        }
        let rels: Vec<String> = paths.iter().map(|p| rel_of(p, &root)).collect();
        let graph = rustscan::module_graph(&rels, &mod_decls, &uses);
        // the module graph keys are rels to the binary's computed root — the
        // orchestrator's scan set is repo-relative, so re-anchor the findings
        // to absolute paths (resolution-neutral, like the per-file findings)
        let mut cycle_findings = graph_families::cycle_findings_for(&graph);
        for f in &mut cycle_findings {
            if !f.file.starts_with('/') {
                f.file = format!("{}/{}", root, f.file);
            }
        }
        all_findings.extend(cycle_findings);
    }
    // repo-wide families from the graph contract + git churn (orchestrator
    // gathers both; the findings compute here)
    let contract = graph_path
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<graph_families::GraphContract>(&s).ok());
    // the contract version is part of the interface — a mismatched version
    // means the exporter and the scan core disagree; degrade to the
    // non-graph families rather than trusting unknown shapes
    if contract.as_ref().is_some_and(|c| c.contract_version != 1) {
        eprintln!(
            "graph contract version {} — expected 1; graph families skipped",
            contract.as_ref().unwrap().contract_version
        );
        // drop the contract so the graph families are skipped
    }
    let contract = if contract.as_ref().is_some_and(|c| c.contract_version == 1) {
        contract
    } else {
        None
    };
    if let Some(c) = &contract {
        let repo_root = Path::new(&root);
        let mut max_cc_by_file: std::collections::HashMap<String, (usize, usize, String)> =
            std::collections::HashMap::new();
        let mut cc_by_file: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for e in &all_cc {
            let rel = rel_of(&e.file, &root);
            *cc_by_file.entry(rel.clone()).or_default() = (*cc_by_file.get(&rel).unwrap_or(&0)).max(e.cc);
            let entry = max_cc_by_file
                .entry(rel)
                .or_insert((e.line, e.cc as usize, e.function.clone()));
            if (e.cc as usize) > entry.1 {
                *entry = (e.line, e.cc as usize, e.function.clone());
            }
        }
        all_findings.extend(graph_families::large_function_findings(
            repo_root,
            c,
            120,
            include_tests,
        ));
        all_findings.extend(graph_families::hub_file_findings(
            repo_root,
            c,
            150,
            include_tests,
            &max_cc_by_file,
        ));
        all_findings.extend(graph_families::high_risk_findings(repo_root, c, 0.8, include_tests));
        all_findings.extend(graph_families::cycle_findings(repo_root, c));
        let rels: Vec<String> = paths.iter().map(|p| rel_of(p, &root)).collect();
        all_findings.extend(graph_families::layer_mix_findings(repo_root, c, &rels));
        all_findings.extend(graph_families::folder_mix_findings(repo_root, c));
        all_findings.extend(graph_families::module_cohesion_findings(repo_root, c, 150));
    }
    if let Some(d) = &docs_root {
        let gitignored: std::collections::HashSet<String> = gitignored_json
            .as_ref()
            .and_then(|j| serde_json::from_str::<Vec<String>>(j).ok())
            .map(|v| v.into_iter().collect())
            .unwrap_or_default();
        all_findings.extend(docs::docs_findings(Path::new(d), &gitignored));
    }
    all_findings.extend(abstraction_findings(&class_scans));
    if let Some(p) = &churn_path {
        if let Ok(s) = std::fs::read_to_string(p) {
            if let Ok(churn) = serde_json::from_str::<std::collections::HashMap<String, usize>>(&s) {
                // max CC per file from the scan itself — recompute without the graph
                let mut cc_any: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
                for e in &all_cc {
                    let rel = rel_of(&e.file, &root);
                    let slot = cc_any.entry(rel).or_insert(0);
                    *slot = (*slot).max(e.cc);
                }
                all_findings.extend(graph_families::hotspot_findings(
                    &churn,
                    &cc_any,
                    0.1,
                    15,
                    &std::collections::HashMap::new(),
                ));
                // churn-untested: top-churn files with no coverage — needs the
                // graph contract (TESTED_BY edges) alongside the churn map
                if let Some(c) = &contract {
                    all_findings.extend(graph_families::churn_untested_findings(
                        Path::new(&root),
                        &churn,
                        c,
                        0.1,
                    ));
                }
            }
        }
    }
    // complexity findings from the scan's own CC array (the gate threshold)
    let mut contract_complexity: Vec<serde_json::Value> = Vec::new();
    for e in &all_cc {
        if e.cc >= 15 {
            contract_complexity.push(serde_json::json!({
                "file": e.file,
                "line": e.line,
                "function": e.function,
                "kind": "complexity",
                "severity": "fail",
                "metric": e.cc,
                "message": common::full_fix_command(
                    &e.file,
                    e.line,
                    &common::complexity_message(e.cc, e.shape, &e.shape_detail),
                ),
            }));
        }
    }
    // the contract: schema_version + final action kinds
    // repo-wide families (duplicate/unused/docs/graph/abstraction) were
    // appended after the per-file suppression passes — filter them through
    // each file's suppressions now so `lucidlint: ignore <signal> <why>`
    // exempts them too (PRD R18)
    // which (file, line, signal) suppressions the repo-wide retain actually
    // used — a stale-suppression finding for one of those is wrong (the
    // suppression IS used; the per-file pass just ran before this filter).
    // The same family-aware, widened-window rules as the per-file pass
    // (filter_repo_wide): `ignore duplicate <why>` two lines above a
    // duplicate exempts it AND the stale finding it caused is dropped
    // (review-log B3 wired for ALL repo-wide families, not just unused).
    let mut used_ln: std::collections::HashSet<(usize, String)> = std::collections::HashSet::new();
    let mut used_fl: std::collections::HashSet<String> = std::collections::HashSet::new();
    let retained: Vec<Finding> = all_findings
        .into_iter()
        .filter(|f| match supps_by_rel.get(&f.file) {
            Some(supps) => {
                let kept = common::filter_repo_wide(vec![f.clone()], supps, &mut used_ln, &mut used_fl);
                !kept.is_empty() // false = suppressed (recorded) + dropped; true = keep
            }
            None => true,
        })
        .collect();
    // drop stale-suppression findings for suppressions the retain just used
    let filtered: Vec<Finding> = retained
        .into_iter()
        .filter(|f| {
            if f.kind != "stale-suppression" {
                return true;
            }
            let sig = sig_of_stale(&f.message);
            let used = used_ln.contains(&(f.line, sig.clone())) || used_fl.contains(&sig);
            !used
        })
        .collect();
    all_findings = filtered;

    let contract_findings: Vec<serde_json::Value> = all_findings
        .iter()
        .map(|f| {
            serde_json::json!({
                "file": f.file,
                "line": f.line,
                "function": f.function,
                "kind": final_kind(&f.kind),
                "signal": f.kind,
                "severity": f.severity,
                "message": common::full_fix_command(&f.file, f.line, &f.message),
            })
        })
        .collect();
    let out = serde_json::json!({
        "schema_version": 2,
        "files": paths.len(),
        "parse_errors": total_errors,
        "findings": contract_findings,
        "cc": all_cc,
        "complexity": contract_complexity,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_fix_command_never_fabricates_name_from_prose() {
        // the plain complexity directive is machine form (kind only) — the
        // rewritten command must carry NO fabricated --name (the regression:
        // the prose "--fix-name <name>" slot started a token with a
        // semicolon and produced "--name apply" for every plain-shape
        // complexity finding)
        let msg = common::complexity_message(16, "plain", "x");
        let cmd = common::full_fix_command("src/a.py", 5, &msg);
        assert!(cmd.contains("--kind extract-method"), "{cmd}");
        assert!(!cmd.contains("--name"), "{cmd}");
        assert!(!cmd.contains("apply"), "{cmd}");
        // name-bearing directives keep their exact placeholder slot
        let m2 = common::full_fix_command(
            "src/a.py",
            5,
            "magic number 3 \u{2014} name it as a constant \u{2014} fix: magic-number --fix-name <CONST>",
        );
        assert!(m2.contains("--name <CONST>"), "{m2}");
        // a prose token merely STARTING with --fix-name is ignored (exact match)
        let m3 = common::full_fix_command(
            "src/a.py",
            5,
            "x \u{2014} fix: extract-method (preview; --fix-name; prose)",
        );
        assert!(!m3.contains("--name"), "{m3}");
    }
    #[test]
    fn repo_root_resolves_relative_paths_against_cwd() {
        // the orchestrator passes REPO-RELATIVE paths with the binary's cwd
        // at the repo root: the raw common prefix of tests/a.py + tools/b.py
        // is "t" — which used to back off to "" and silently broke every
        // graph-family rel resolution (large-function/hub-file/high-risk/
        // layer-mix/folder-mix never fired through the orchestrator)
        let cwd = Path::new("/work/repo");
        let root = repo_root_impl(
            &[
                "tests/a.py".to_string(),
                "tests/b.py".to_string(),
                "tools/c.py".to_string(),
            ],
            cwd,
        );
        assert_eq!(root, "/work/repo");
    }

    #[test]
    fn repo_root_absolute_paths_unchanged() {
        let cwd = Path::new("/work/repo");
        let root = repo_root_impl(
            &[
                "/home/u/proj/tests/a.py".to_string(),
                "/home/u/proj/tools/b.py".to_string(),
            ],
            cwd,
        );
        assert_eq!(root, "/home/u/proj");
    }

    #[test]
    fn fix_command_carries_params_slot() {
        // the extract-module directive names the seam's members via --params —
        // the rewrite must carry them or the agent gets a command the fix
        let msg = "module x holds 2 domains — fix: extract-module --fix-name text --params tokenize,words";

        let cmd = common::full_fix_command("tools/layout.py", 0, msg);
        assert!(cmd.contains("--kind extract-module"), "{cmd}");
        assert!(cmd.contains("--name text"), "{cmd}");
        assert!(cmd.contains("--params tokenize,words"), "{cmd}");
    }

    fn scan_src(src: &str) -> Vec<Finding> {
        scan_corpus(&[("prod_mod.py", src)])
    }

    /// The first top-level function's body — for shape-classifier tests.
    fn first_py_fn_body(src: &str) -> Vec<Stmt> {
        let parsed = parse_module(src).unwrap();
        let module = parsed.syntax();
        match module.body.first().unwrap() {
            Stmt::FunctionDef(f) => f.body.iter().cloned().collect(),
            _ => panic!("the first statement is not a function"),
        }
    }

    /// The test-only families (monkeypatch/skipif/fakefs) run for test paths.
    fn scan_src_test(src: &str) -> Vec<Finding> {
        scan_corpus(&[("tests/test_mod.py", src)])
    }

    fn scan_cc(src: &str) -> (Vec<Finding>, Vec<FnCc>, usize) {
        let scan = scan_source(src, "prod_mod.py");
        (scan.findings, scan.cc, scan.errors)
    }

    fn kinds(findings: &[Finding]) -> Vec<&str> {
        let mut v: Vec<&str> = findings.iter().map(|f| f.kind.as_str()).collect();
        v.sort_unstable();
        v
    }

    // ------------------------------------------------------------- CC
    #[test]
    fn cc_if_elif_else_counts_elifs_not_else() {
        let src =
            "def f(a):\n    if a:\n        return 1\n    elif b:\n        return 2\n    else:\n        return 3\n";
        let (_, cc, _) = scan_cc(src);
        assert_eq!(cc[0].cc, 3); // if + elif; trailing else does NOT count
    }

    #[test]
    fn cc_loops_try_assert_match_boolop() {
        let src = "def f(xs, a, b):\n    for x in xs:\n        if x:\n            break\n    else:\n        return 0\n    try:\n        g()\n    except ValueError:\n        h()\n    else:\n        k()\n    assert a and b\n    match a:\n        case 1:\n            return 1\n        case _:\n            return 0\n";
        let (_, cc, _) = scan_cc(src);
        assert_eq!(cc[0].cc, 8); // for+else(2) if(1) try+handler+else(3) assert(1) match-case(1) + base 1 — the boolop under assert does NOT count (radon's visit_Assert never recurses)
    }

    #[test]
    fn cc_nested_and_class_excluded() {
        let src = "def f(a):\n    def inner(x):\n        if x:\n            return 1\n        return 0\n    class C:\n        def m(self):\n            if self:\n                return 1\n    if a:\n        return inner(a)\n    return 0\n";
        let (_, cc, _) = scan_cc(src);
        assert_eq!(cc.len(), 1);
        assert_eq!(cc[0].cc, 2); // only the outer if counts
    }

    #[test]
    fn cc_assert_does_not_recurse() {
        // radon's visit_Assert short-circuits: boolop/ternary/comp inside
        // an assert contribute nothing — only the assert itself counts
        let src = "def f(a, b, y):\n    assert a and b\n    assert [x for x in y if x]\n    return a\n";
        let (_, cc, _) = scan_cc(src);
        assert_eq!(cc[0].cc, 3); // base + 2 asserts; the boolop/comp/ifs do not count
    }

    #[test]
    fn cc_lambda_zero_but_body_walks() {
        let src = "def f():\n    g = lambda x: 1 if x else 2\n    return g(1)\n";
        let (_, cc, _) = scan_cc(src);
        assert_eq!(cc[0].cc, 2); // lambda +0, inner ternary +1
    }

    #[test]
    fn cc_comprehension_counts_each_generator() {
        let src = "def f(xs):\n    return [x for x in xs for y in xs if y]\n";
        let (_, cc, _) = scan_cc(src);
        assert_eq!(cc[0].cc, 4); // 2 generators + 1 if + base
    }

    // ------------------------------------------------------------- magic
    #[test]
    fn magic_skips_lookup_table_and_small_literals() {
        let f = scan_src(
            "def f(a):\n    table = {10: 'x', 20: 'y'}\n    if a > 1:\n        return table[a]\n    return 0\n",
        );
        assert!(!f.iter().any(|x| x.kind == "magic-number")); // 10/20 in a dict literal, 1 and 0 skipped
    }

    #[test]
    fn magic_operand_and_index_found() {
        let f = scan_src("def f(a):\n    return a * 60 + cols[7]\n");
        let m: Vec<&Finding> = f.iter().filter(|x| x.kind == "magic-number").collect();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].function, "f");
    }

    #[test]
    fn magic_keyword_value_is_not_a_finding() {
        let f = scan_src("def f():\n    raise HTTPException(status_code=403, detail='x')\n");
        assert!(!f.iter().any(|x| x.kind == "magic-number"));
    }

    #[test]
    fn positional_boolean_literals_are_found_keyword_exempt() {
        let f = scan_src("def f():\n    retry(g(True), h(retry=True), False)\n    return 1\n");
        let b: Vec<&Finding> = f.iter().filter(|x| x.kind == "boolean-arg").collect();
        assert_eq!(b.len(), 2); // g(True) positional and outer False; retry=True keyword exempt
        assert!(b.iter().all(|x| x.message.contains("name it")));
    }

    #[test]
    fn boolean_name_and_keyword_args_pass() {
        let f = scan_src("def f(flag):\n    run(flag, retry=True, cache=False)\n    return 1\n");
        assert!(!f.iter().any(|x| x.kind == "boolean-arg"));
    }

    #[test]
    fn magic_in_nested_function_attributes_to_innermost() {
        let f = scan_src("def outer():\n    def inner():\n        return rate * 60\n    return inner()\n");
        let m: Vec<&Finding> = f.iter().filter(|x| x.kind == "magic-number").collect();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].function, "inner");
    }

    // ------------------------------------------------------------- noop
    #[test]
    fn noop_ternary_is_a_finding() {
        let f = scan_src("def f(payload, postcode):\n    payload.address if is_outcode(postcode) else postcode\n    return postcode\n");
        assert!(f.iter().any(|x| x.kind == "noop-statement"));
    }

    #[test]
    fn noop_calls_and_docstrings_pass() {
        let f = scan_src("def f():\n    \"\"\"docstring\"\"\"\n    cleanup()\n    (x := 1)\n    return x\n");
        assert!(!f.iter().any(|x| x.kind == "noop-statement"));
    }

    // ------------------------------------------------------------- imports
    #[test]
    fn inline_import_in_function_and_method() {
        let f = scan_src("def f():\n    import os\n    return os.path\n\nclass C:\n    def m(self):\n        import sys\n        return sys\n");
        let inline: Vec<&Finding> = f.iter().filter(|x| x.kind == "inline-import").collect();
        assert_eq!(inline.len(), 2);
    }

    #[test]
    fn private_import_both_forms_and_future_skip() {
        let f = scan_src("from __future__ import annotations\nimport pkg._internal\nfrom houses import _secret\n");
        let priv_imports: Vec<&Finding> = f.iter().filter(|x| x.kind == "private-import").collect();
        assert_eq!(priv_imports.len(), 2); // __future__ skipped
    }

    // ------------------------------------------------------------- unreachable
    #[test]
    fn unreachable_after_return() {
        let f = scan_src("def f():\n    return 1\n    x = 2\n");
        assert!(f.iter().any(|x| x.kind == "unreachable" && x.line == 3));
    }

    #[test]
    fn unreachable_inside_nested_if_not_flagged() {
        let f = scan_src("def f(a):\n    if a:\n        return 1\n    return 0\n");
        assert!(!f.iter().any(|x| x.kind == "unreachable"));
    }

    // ------------------------------------------------------------- suppressions
    #[test]
    fn suppression_with_why_exempts() {
        let f = scan_src("def f():\n    try:\n        g()\n    except ValueError:  # lucidlint: ignore swallow this is safe, logged\n        log('x')\n");
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn suppression_without_why_is_a_finding() {
        let f = scan_src(
            "def f():\n    try:\n        g()\n    except ValueError:  # lucidlint: ignore swallow\n        log('x')\n",
        );
        assert!(f.iter().any(|x| x.kind == "suppression"));
        assert!(f.iter().any(|x| x.kind == "swallow")); // not actually exempted
    }

    #[test]
    fn suppression_on_line_above_exempts() {
        // the comment must sit on the finding's line or line-1 — the except
        // handler is line 5, so line 4 (the try) is the line-1 position
        let f = scan_src("def f():\n    try:\n        g()\n    # lucidlint: ignore swallow deliberate skip\n    except ValueError:\n        log('x')\n");
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn ignore_file_exempts() {
        let src = "# lucidlint: ignore-file class-module helper inside a CLI utility\nclass Helper:\n    def run(self):\n        return 1\n";
        let f = scan_src(src);
        assert!(!f.iter().any(|x| x.kind == "class-module"));
    }

    // ------------------------------------------------------------- type-ignore
    #[test]
    fn type_ignore_without_why_is_a_finding() {
        let f = scan_src("x: int = 1  # type: ignore\n");
        assert!(f.iter().any(|x| x.kind == "type-ignore" && x.line == 1));
    }

    #[test]
    fn type_ignore_with_why_passes() {
        let f = scan_src("x: int = 1  # type: ignore # pyright cannot see the kwarg\n");
        assert!(!f.iter().any(|x| x.kind == "type-ignore"));
    }

    #[test]
    fn noqa_without_why_is_a_finding() {
        let f = scan_src("x = 1  # noqa\n");
        assert!(f.iter().any(|x| x.kind == "noqa" && x.line == 1));
    }

    #[test]
    fn pragma_no_cover_without_why_is_a_finding() {
        let f = scan_src("x = 1  # pragma: no cover\n");
        assert!(f.iter().any(|x| x.kind == "noqa"));
    }

    #[test]
    fn noqa_with_why_passes() {
        let f = scan_src("x = 1  # noqa # mypy cannot see the overload\n");
        assert!(!f.iter().any(|x| x.kind == "noqa"));
    }

    #[test]
    fn noqa_reason_on_the_same_comment_line_passes() {
        // the repo writes `# noqa: BLE001 — the callback must never 500` —
        // one comment with a real reason. The rule's requirement of a SECOND
        // `#` after the marker (`# noqa: X  # reason`) rejects that natural
        // format: a reason is a reason whether or not it follows a `#`.
        let f = scan_src("try:\n    pass\nexcept Exception:  # noqa: BLE001 — the callback must never 500\n    pass\n");
        assert!(!f.iter().any(|x| x.kind == "noqa"), "{f:?}");
        // and the no-reason form still fails
        let g = scan_src("x = 1  # noqa: BLE001\n");
        assert!(g.iter().any(|x| x.kind == "noqa"));
    }

    // ------------------------------------------------------------- global-state
    #[test]
    fn module_literal_assign_and_annassign_are_findings() {
        let f = scan_src("state = []\n_oauth_states: dict = {}\ndef f():\n    return 1\n");
        let gs: Vec<&Finding> = f.iter().filter(|x| x.kind == "global-state").collect();
        assert_eq!(gs.len(), 2);
    }

    #[test]
    fn negative_literals_stay_constant_tables() {
        let f = scan_src("DEFAULT_BBOX = {'lat_min': 50.1, 'lon_min': -4.0}\ndef f():\n    return DEFAULT_BBOX\n");
        assert!(!f.iter().any(|x| x.kind == "global-state"));
    }

    #[test]
    fn mutation_of_flagged_literal_not_duplicated() {
        let f = scan_src("state = []\ndef f():\n    state.append(1)\n");
        let gs: Vec<&Finding> = f.iter().filter(|x| x.kind == "global-state").collect();
        assert_eq!(gs.len(), 1); // the literal, not the mutation
    }

    // ------------------------------------------------------------- builtin-shadow
    #[test]
    fn shadow_params_and_locals() {
        let f = scan_src("def f(list, id):\n    str = 'x'\n    return list\n");
        let sh: Vec<&Finding> = f.iter().filter(|x| x.kind == "builtin-shadow").collect();
        assert_eq!(sh.len(), 3);
    }

    // ------------------------------------------------------------- signature hygiene
    #[test]
    fn long_param_list_is_found() {
        let f = scan_src("def f(a, b, c, d, e, g, h):\n    return a\n");
        let lp: Vec<&Finding> = f.iter().filter(|x| x.kind == "long-param-list").collect();
        assert_eq!(lp.len(), 1);
        assert!(lp[0].message.contains("7 parameters"));
    }

    #[test]
    fn long_param_list_counts_leading_self_out() {
        let f = scan_src("class C:\n    def m(self, a, b, c, d, e, f):\n        return a\n");
        let lp: Vec<&Finding> = f.iter().filter(|x| x.kind == "long-param-list").collect();
        assert_eq!(lp.len(), 1);
        assert!(lp[0].message.contains("6 parameters"));
    }

    #[test]
    fn short_param_list_and_five_plus_self_pass() {
        let f = scan_src(
            "def f(a, b, c, d, e):\n    return a\n\nclass C:\n    def m(self, a, b, c, d, e):\n        return a\n",
        );
        assert!(!f.iter().any(|x| x.kind == "long-param-list"));
    }

    // ------------------------------------------------------------- except family
    #[test]
    fn bare_and_log_only_swallows() {
        let f = scan_src("def f():\n    try:\n        g()\n    except:\n        pass\n    try:\n        h()\n    except ValueError:\n        log('x')\n");
        assert_eq!(f.iter().filter(|x| x.kind == "swallow").count(), 2);
    }

    #[test]
    fn surfaced_return_is_not_a_swallow() {
        let f = scan_src("def f():\n    try:\n        g()\n    except ValueError:\n        return 'failed'\n");
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn accumulator_surfacing_via_returned_name() {
        let f = scan_src("def validate(rows):\n    issues = []\n    for s in rows:\n        try:\n            parse(s)\n        except ValueError as e:\n            issues.append(str(e))\n    return issues\n");
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn sys_exit_surfaces() {
        let f = scan_src("import sys\ndef f():\n    try:\n        data = parse()\n    except ValueError:\n        sys.stderr.write('bad')\n        sys.exit(2)\n    return data\n");
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn real_tfl_build_cost_groups_counts_two() {
        // faithful slice of houses/tfl_client.py:583 — nested def + one
        // keyword-position lambda must count 2 (Python ast.walk parity)
        let src = "def _build_cost_groups(self, data):\n    journeys = data.get(\"journeys\", [])\n    if not journeys:\n        return []\n    best = min(journeys, key=lambda j: j.get(\"duration\", 9999))\n    mode_single_pence = {}\n    current_legs = []\n\n    def _flush_transit():\n        nonlocal current_legs\n        if not current_legs:\n            return\n        return [g for g in current_legs]\n\n    return best\n";
        let parsed = ruff_python_parser::parse_module(src).unwrap();
        let body = parsed.syntax().body.clone();
        let inner = checks::inner_function_count(&body[0]);
        assert_eq!(inner, 2, "real fn inner count: {inner}");
    }

    #[test]
    fn keyword_lambdas_count_towards_closures() {
        // METHOD: cc gate = 0 (fn_map semantics); span >= 60 via a padding
        // comment block; two keyword-position lambdas → closure fires
        let pad = "    # padding\n".repeat(58);
        let f = scan_src(&format!(
            "class C:\n    def _build_cost_groups(self, data):\n        journeys = data.get(\"journeys\", [])\n        if not journeys:\n            return []\n        best = min(journeys, key=lambda j: j.get(\"duration\", 9999))\n        groups = sorted(journeys, key=lambda j: j.get(\"mode\", \"\"))\n{pad}        return best, groups\n"
        ));
        let closures: Vec<(usize, &str)> = f
            .iter()
            .filter(|x| x.kind == "closures")
            .map(|x| (x.line, x.message.as_str()))
            .collect();
        assert_eq!(closures.len(), 1, "expected one closure, got {closures:?}");
    }

    #[test]
    fn subscript_store_on_returned_name_surfaces() {
        let f = scan_src("def to_json_value(self):\n    result = {}\n    try:\n        g()\n    except Exception:\n        logger.exception('x')\n        result['value'] = None\n    return result\n");
        assert!(
            !f.iter().any(|x| x.kind == "swallow"),
            "expected no swallow, got {:?}",
            f.iter().map(|x| (x.kind.as_str(), x.line)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn broad_except_is_a_warn() {
        let f = scan_src(
            "def f():\n    try:\n        g()\n    except Exception as e:\n        log(e)\n        return fallback\n",
        );
        let b: Vec<&Finding> = f.iter().filter(|x| x.kind == "broad-except").collect();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].severity, "warn");
    }

    // ------------------------------------------------------------- closures
    #[test]
    fn closures_two_inner_functions_with_cc() {
        // cc must reach the >= 15 gate: 14 ifs + 1 base
        let ifs: Vec<String> = (0..14).map(|i| format!("    if a{i}:\n        x = {i}\n")).collect();
        let src = format!(
            "def big(a):\n    def inner_a(x):\n        return x\n    def inner_b(x):\n        return x\n    x = 0\n{}\n    return x\n",
            ifs.concat()
        );
        let f = scan_src(&src);
        assert!(
            f.iter().any(|x| x.kind == "closures"),
            "expected closures, got {:?}",
            kinds(&f)
        );
    }

    // ------------------------------------------------------------- class-module
    #[test]
    fn class_module_name_mismatch() {
        let f = scan_src("class Config:\n    pass\n");
        assert!(f.iter().any(|x| x.kind == "class-module"));
    }

    // ------------------------------------------------------------- strewing
    #[test]
    fn strewing_three_functions_shared_param() {
        let f = scan_src("class Record:\n    pass\n\ndef a(x: Record):\n    return x\n\ndef b(x: Record):\n    return x\n\ndef c(x: Record):\n    return x\n");
        assert!(f.iter().any(|x| x.kind == "strewing"));
    }

    // ------------------------------------------------------------- duplicate-def
    #[test]
    fn duplicate_module_scope_def_is_found() {
        // the review-log guess_pages shadow: two defs, same name, module scope
        let f = scan_src("def guess_pages():\n    return 1\n\ndef guess_pages(model):\n    return model\n");
        let d: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-def").collect();
        assert_eq!(d.len(), 1, "{f:?}");
        assert_eq!(d[0].line, 4, "the SECOND binding is the finding");
    }

    #[test]
    fn duplicate_class_and_function_name_is_found() {
        // def shadowing a class of the same name is the same hazard
        let f = scan_src("class Record:\n    pass\n\ndef Record():\n    return 1\n");
        assert!(f.iter().any(|x| x.kind == "duplicate-def"));
    }

    #[test]
    fn duplicate_def_import_shadow_is_found() {
        // a def whose name collides with an import (the def-in-imports edit
        // mistake class)
        let f = scan_src("from x import helper\n\ndef helper():\n    return 1\n");
        assert!(f.iter().any(|x| x.kind == "duplicate-def"));
    }

    #[test]
    fn duplicate_def_suppressed_with_why() {
        let f = scan_src(
            "def a():\n    return 1\n\n# lucidlint: ignore duplicate-def the override is deliberate\n\ndef a(x):\n    return x\n",
        );
        assert!(!f.iter().any(|x| x.kind == "duplicate-def"), "{f:?}");
    }

    #[test]
    fn duplicate_def_overload_battery_exempt() {
        // the @overload idiom legally binds one name several times — the
        // stubs plus the implementation; "rename one" is unfollowable there
        let f = scan_src(
            "from typing import overload\n\n@overload\ndef f(x: int) -> int:\n    ...\n\n@overload\ndef f(x: str) -> str:\n    ...\n\ndef f(x):\n    return x\n",
        );
        assert!(!f.iter().any(|x| x.kind == "duplicate-def"), "{f:?}");
    }

    #[test]
    fn duplicate_def_overload_aliased_decorator_exempt() {
        // the exemption resolves the BOUND name: `overload as ov` binds ov,
        // and @ov stubs + impl are the legal idiom (review finding)
        let f = scan_src(
            "from typing import overload as ov\n\n@ov\ndef f(x: int) -> int:\n    ...\n\n@ov\ndef f(x: str) -> str:\n    ...\n\ndef f(x):\n    return x\n",
        );
        assert!(!f.iter().any(|x| x.kind == "duplicate-def"), "{f:?}");
    }

    #[test]
    fn duplicate_def_overload_impl_duplicate_still_flagged() {
        // stubs + impl are exempt — a FOURTH def of the same name after the
        // impl is a genuine duplicate and must still fire (review finding)
        let f = scan_src(
            "from typing import overload\n\n@overload\ndef f(x: int) -> int:\n    ...\n\ndef f(x):\n    return x\n\ndef f(x):\n    return x + 1\n",
        );
        assert!(f.iter().any(|x| x.kind == "duplicate-def"), "{f:?}");
    }
    #[test]
    fn restating_docstring_is_found() {
        // the log's example: "the line's orientation must be consistent with
        // its box's aspect" beside code that says exactly that — every content
        // word is an identifier already in the body
        let f = scan_src(
            "def check(box, line):\n    \"\"\"the line orientation must be consistent with the box aspect\"\"\"\n    orientation = line.orientation\n    consistent = box.aspect\n    return orientation == consistent\n",
        );
        assert!(f.iter().any(|x| x.kind == "restating-docstring"), "{f:?}");
    }

    #[test]
    fn meaningful_docstring_passes() {
        // the docstring names what the body does not: the CONCEPT
        let f = scan_src(
            "def check(box, line):\n    \"\"\"admission gate: the reading order must stay on the ink axis\"\"\"\n    orient = line.orientation\n    aspect = box.aspect\n    return orient == aspect\n",
        );
        assert!(!f.iter().any(|x| x.kind == "restating-docstring"), "{f:?}");
    }

    // ------------------------------------------------------------- duplicate-block
    #[test]
    fn adjacent_duplicate_block_is_found() {
        // the transcribe-twice class: a replaced loop header leaves the old
        // body in place — two identical transcribe->write->mark sequences
        let f = scan_src(
            "def run(pages):\n    for p in pages:\n        t = transcribe(p)\n        write(t)\n        mark(p)\n    t = transcribe(p)\n    write(t)\n    mark(p)\n",
        );
        assert!(f.iter().any(|x| x.kind == "duplicate-block"), "{f:?}");
    }

    #[test]
    fn short_duplicate_pair_passes() {
        // two identical statements are common and often fine — 3+ is the
        // edit-mistake signature
        let f = scan_src("def f(a):\n    a = a + 1\n    a = a + 1\n    return a\n");
        assert!(!f.iter().any(|x| x.kind == "duplicate-block"), "{f:?}");
    }

    // ------------------------------------------------- suppression scope
    #[test]
    fn suppression_scoped_to_its_line() {
        // an explained ignore on one except does not exempt a second except
        let f = scan_src("def f():\n    try:\n        g()\n    except ValueError:  # lucidlint: ignore swallow this one is safe, logged\n        log('a')\n    try:\n        h()\n    except ValueError:\n        log('b')\n");
        let exc: Vec<&Finding> = f.iter().filter(|x| x.kind == "swallow").collect();
        assert_eq!(exc.len(), 1);
        assert_eq!(exc[0].line, 8); // the second handler (line 8) is not exempted
    }

    #[test]
    fn suppression_wrong_signal_does_not_exempt() {
        let f = scan_src("def f():\n    try:\n        g()\n    except ValueError:  # lucidlint: ignore inline-import not the right signal\n        log('skipping')\n");
        assert!(f.iter().any(|x| x.kind == "swallow")); // still a swallow; an explained mis-scoped ignore emits no suppression finding
    }

    #[test]
    fn ignore_file_without_why_is_a_finding() {
        let src = "# lucidlint: ignore-file except\ndef f():\n    try:\n        g()\n    except ValueError:\n        log('x')\n";
        let f = scan_src(src);
        assert!(f.iter().any(|x| x.kind == "swallow")); // not exempted
        assert!(f.iter().any(|x| x.kind == "suppression"));
    }

    #[test]
    fn type_ignore_in_docstring_is_not_a_finding() {
        let f =
            scan_src("def f():\n    \"\"\"Never silence: type: ignore lives in real comments.\"\"\"\n    return 1\n");
        assert!(!f.iter().any(|x| x.kind == "type-ignore"));
    }

    // ------------------------------------------------- except edges
    #[test]
    fn except_with_raise_is_not_a_swallow() {
        let f =
            scan_src("def f():\n    try:\n        g()\n    except ValueError:\n        log('bad')\n        raise\n");
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn except_returning_empty_dict_is_not_a_swallow() {
        let f = scan_src("def f():\n    try:\n        g()\n    except ValueError:\n        return {}\n");
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn except_return_none_is_not_a_swallow() {
        let f = scan_src("def f():\n    try:\n        g()\n    except ValueError:\n        return None\n");
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn except_continue_is_not_a_swallow() {
        let f = scan_src("def f(rows):\n    for r in rows:\n        try:\n            parse(r)\n        except ValueError:\n            continue\n    return 1\n");
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn empty_exception_catch_still_fails() {
        let f = scan_src("def f():\n    try:\n        g()\n    except Exception:\n        pass\n");
        assert!(f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn accumulator_not_returned_still_swallows() {
        let f = scan_src("def f(rows):\n    issues = []\n    try:\n        parse(rows)\n    except ValueError as e:\n        issues.append(str(e))\n    return 'done'\n");
        assert!(f.iter().any(|x| x.kind == "swallow")); // issues not returned → swallow
    }

    // ------------------------------------------------- global-state edges
    #[test]
    fn constant_table_mutated_in_function_is_still_state() {
        // the all-constant literal passes at module level (carve-out), but a
        // function mutation of the container is still module state
        let f = scan_src("LOOKUP = {'a': 1}\ndef f(k):\n    LOOKUP[k] = 2\n");
        let gs: Vec<&Finding> = f.iter().filter(|x| x.kind == "global-state").collect();
        assert_eq!(gs.len(), 1);
        assert_eq!(gs[0].line, 3);
    }

    // ------------------------------------------------- class-module pass
    #[test]
    fn class_module_matching_name_and_multi_class_pass() {
        let f = scan_src("class User:\n    def name(self):\n        return 'u'\n\nclass Team:\n    def name(self):\n        return 't'\n");
        assert!(!f.iter().any(|x| x.kind == "class-module"));
    }

    // ------------------------------------------------- shadow pass
    #[test]
    fn no_builtin_shadow_in_clean_fn() {
        let f = scan_src("def f(a, b):\n    return a + b\n");
        assert!(!f.iter().any(|x| x.kind == "shadow"));
    }

    // ------------------------------------------------- vague-name
    #[test]
    fn vague_name_thin_role_class_passes() {
        let f = scan_src("class PaymentsHandler:\n    def __init__(self, svc):\n        self.svc = svc\n    def handle(self, evt):\n        return self.svc.process(evt)\n");
        assert!(!f.iter().any(|x| x.kind == "vague-name"));
    }

    #[test]
    fn vague_name_load_bearing_class_is_found() {
        let mut src = String::from("class DataManager:\n    def __init__(self):\n        self.store = {}\n");
        for i in 0..6 {
            src.push_str(&format!("    def method_{i}(self):\n        return {i}\n"));
        }
        let f = scan_src(&src);
        assert!(f.iter().any(|x| x.kind == "vague-name"));
    }

    // ------------------------------------------------- severity
    // ------------------------------------------- docs + abstraction
    #[test]
    fn docs_broken_link_is_found() {
        let dir = std::env::temp_dir().join(format!("docs_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(
            dir.join("docs/guide.md"),
            "see [missing](nope.md)
",
        )
        .unwrap();
        let f = docs::docs_findings(&dir, &std::collections::HashSet::new());
        assert!(f.iter().any(|x| x.kind == "docs-link" && x.message.contains("nope.md")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abstraction_single_concrete_is_found() {
        let src_a = "from abc import ABC, abstractmethod\n\nclass Base(ABC):\n    @abstractmethod\n    def run(self):\n        pass\n";
        let src_b = "from a import Base\n\nclass Concrete(Base):\n    def run(self):\n        return 1\n";
        let scan_a = scan_source(src_a, "a.py");
        let scan_b = scan_source(src_b, "b.py");
        let scans = vec![
            ("a.py".to_string(), scan_a.classes, scan_a.imports),
            ("b.py".to_string(), scan_b.classes, scan_b.imports),
        ];
        let f = checks::abstraction_findings(&scans);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].function, "Base");
    }

    #[test]
    fn abstraction_two_subclasses_passes() {
        let src_a = "from abc import ABC, abstractmethod\n\nclass Base(ABC):\n    @abstractmethod\n    def run(self):\n        pass\n";
        let src_b = "from a import Base\n\nclass One(Base):\n    def run(self):\n        return 1\n\nclass Two(Base):\n    def run(self):\n        return 2\n";
        let scan_a = scan_source(src_a, "a.py");
        let scan_b = scan_source(src_b, "b.py");
        let scans = vec![
            ("a.py".to_string(), scan_a.classes, scan_a.imports),
            ("b.py".to_string(), scan_b.classes, scan_b.imports),
        ];
        let f = checks::abstraction_findings(&scans);
        assert!(f.is_empty());
    }

    // ------------------------------------------------- record-shape
    #[test]
    fn record_grab_bag_and_collection_params_fail() {
        let f = scan_src(
            "def f(m: dict[str, Any]):\n    return m\n\ndef g(rows: list[dict[str, str]]):\n    return rows\n",
        );
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn record_map_and_domain_params_pass() {
        let f = scan_src("def f(counts: dict[str, int], items: list[Item]):\n    return counts, items\n");
        assert!(!f.iter().any(|x| x.kind == "record-shape"));
    }

    #[test]
    fn record_return_collection_of_dicts_fails() {
        let f = scan_src("def f() -> dict[str, dict[str, int]]:\n    return {}\n");
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1);
        assert!(r[0].message.contains("as return type"));
    }

    #[test]
    fn record_variadic_tuple_and_fixed_tuple() {
        // tuple[str, ...] is a sequence; tuple[str, int] is a record pair
        let f = scan_src(
            "def a(x: tuple[str, ...]) -> None:\n    pass\n\ndef b(pair: tuple[str, int]) -> None:\n    pass\n",
        );
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1);
        assert!(r[0].message.contains("pair"));
    }

    #[test]
    fn record_deserializer_boundary_is_exempt() {
        // raw JSON in, domain class out — the sanctioned bare-dict spot
        let f = scan_src("def parse(raw: dict[str, Any]) -> Label:\n    return Label(raw)\n");
        assert!(!f.iter().any(|x| x.kind == "record-shape"));
    }

    #[test]
    fn record_dict_literal_in_return_is_found() {
        let f = scan_src("def f(x):\n    return {\"kind\": \"tool_call\", \"value\": x}\n");
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1);
        assert!(r[0].message.contains("dict with constant keys"));
        assert_eq!(r[0].line, 2);
    }

    #[test]
    fn record_inline_call_args_and_lookup_tables_pass() {
        let f = scan_src("def f(x):\n    client.post(url, headers={\"Content-Type\": \"json\", \"X\": x})\n    table = {\"a\": 1, \"b\": 2}\n    return table\n");
        assert!(!f.iter().any(|x| x.kind == "record-shape"));
    }

    #[test]
    fn record_spread_merge_is_not_a_record() {
        let f = scan_src("def f(session, x):\n    return {**session, \"x\": x}\n");
        assert!(!f.iter().any(|x| x.kind == "record-shape"));
    }

    #[test]
    fn record_dict_call_in_return_is_found() {
        // dict(a=1, b=x) is the literal's call-form twin — the bypass a
        // literal-only scan left open
        let f = scan_src("def f(x):\n    return dict(kind=\"tool_call\", value=x)\n");
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert_eq!(r[0].line, 2);
    }

    #[test]
    fn record_dict_call_all_constant_passes() {
        // a lookup, not a record — same exemption as the literal form
        let f = scan_src("def f():\n    return dict(a=1, b=2)\n");
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
    }

    #[test]
    fn record_dict_call_as_inline_argument_passes() {
        // inline call arguments are maps — not record positions
        let f = scan_src("def f(x):\n    client.post(dict(a=1, b=x))\n");
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
    }

    #[test]
    fn record_dict_call_wrapping_literal_is_found() {
        // dict({"a": 1, "b": x}) — the inner literal is the record
        let f = scan_src("def f(x):\n    return dict({\"a\": 1, \"b\": x})\n");
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1, "{f:?}");
    }

    // ------------------------------------------- partition + test families
    #[test]
    fn partition_field_disjoint_groups_is_found() {
        // 6 methods, >= 150 lines: group A touches a/b, group B touches c/d
        let mut src = String::from("class Manager:\n    def __init__(self):\n        self.a = 0\n");
        for i in 0..3 {
            src.push_str(&format!(
                "    def group_a_{i}(self):\n        x = self.a + self.b\n        for k in range(x):\n            x += k\n        return x\n"
            ));
        }
        for i in 0..3 {
            src.push_str(&format!(
                "    def group_b_{i}(self):\n        x = self.c + self.d\n        for k in range(x):\n            x += k\n        return x\n"
            ));
        }
        // span >= 150 code lines: a long docstring is a node (end_lineno
        // stops at the last node, comments don't count)
        src.push_str("    def _pad(self):\n        \"\"\"");
        for _ in 0..140 {
            src.push_str("padding line\n        ");
        }
        src.push_str("\"\"\"\n        return 1\n");
        let f = scan_src(&src);
        assert!(f.iter().any(|x| x.kind == "partition"));
    }

    #[test]
    fn partition_small_class_passes() {
        let f = scan_src("class Small:\n    def a(self):\n        return 1\n    def b(self):\n        return 2\n");
        assert!(!f.iter().any(|x| x.kind == "partition"));
    }

    #[test]
    fn monkeypatch_setattr_and_patch_decorator_found() {
        let f = scan_src_test("from unittest import mock\n\ndef test_x(monkeypatch):\n    monkeypatch.setattr(Obj, 'x', 1)\n\n@mock.patch('y')\ndef test_y():\n    pass\n");
        let m: Vec<&Finding> = f.iter().filter(|x| x.kind == "monkeypatch").collect();
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn skipif_on_environment_is_found() {
        let f = scan_src_test("import pytest\nimport os\n\n@pytest.mark.skipif('os.environ' in os.environ, reason='env')\ndef test_x():\n    pass\n");
        assert!(f.iter().any(|x| x.kind == "skipif"));
    }

    #[test]
    fn bare_pytest_mark_skip_is_found() {
        let f = scan_src_test("import pytest\n\n@pytest.mark.skip\ndef test_x():\n    pass\n");
        assert!(f.iter().any(|x| x.kind == "skipif"));
    }

    #[test]
    fn pytest_mark_skip_with_parens_is_found_once() {
        let f = scan_src_test("import pytest\n\n@pytest.mark.skip()\ndef test_x():\n    pass\n");
        let s: Vec<&Finding> = f.iter().filter(|x| x.kind == "skipif").collect();
        assert_eq!(s.len(), 1); // the Call node and its func Attribute node dedupe
    }

    #[test]
    fn other_markers_and_env_free_skipif_pass() {
        let f = scan_src_test("import pytest\n\n@pytest.mark.parametrize('x', [1, 2])\ndef test_x(x):\n    assert x\n\n@pytest.mark.skipif(True, reason='tmp')\ndef test_y():\n    pass\n");
        assert!(!f.iter().any(|x| x.kind == "skipif"));
    }

    #[test]
    fn fakefs_real_fs_without_pyfakefs_is_found() {
        let f = scan_src_test("def test_x(tmp_path):\n    p = tmp_path / 'a'\n    p.write_text('hi')\n");
        assert!(f.iter().any(|x| x.kind == "fakefs"));
    }

    #[test]
    fn fakefs_sanctioned_subprocess_need_passes() {
        let f = scan_src_test("import subprocess\n\ndef test_x(tmp_path):\n    subprocess.run(['ls'])\n");
        assert!(!f.iter().any(|x| x.kind == "fakefs"));
    }

    // ------------------------------------------------------------- no-assert-test
    #[test]
    fn test_with_assertion_passes() {
        let f = scan_src_test("def test_x():\n    assert 1 == 1\n");
        assert!(!f.iter().any(|x| x.kind == "no-assert-test"));
    }

    #[test]
    fn test_with_pytest_raises_passes() {
        let f = scan_src_test("import pytest\n\ndef test_x():\n    with pytest.raises(ValueError):\n        g()\n");
        assert!(!f.iter().any(|x| x.kind == "no-assert-test"));
    }

    #[test]
    fn test_with_fail_call_passes() {
        let f = scan_src_test("import pytest\n\ndef test_x():\n    pytest.fail('nope')\n");
        assert!(!f.iter().any(|x| x.kind == "no-assert-test"));
    }

    #[test]
    fn test_with_assert_in_nested_function_passes() {
        let f = scan_src_test("def test_x():\n    def check():\n        assert 1 == 1\n    check()\n");
        assert!(!f.iter().any(|x| x.kind == "no-assert-test"));
    }

    #[test]
    fn test_without_assertion_is_found() {
        let f = scan_src_test("def test_x():\n    setup()\n    teardown()\n");
        let na: Vec<&Finding> = f.iter().filter(|x| x.kind == "no-assert-test").collect();
        assert_eq!(na.len(), 1);
        assert_eq!(na[0].function, "test_x");
        assert!(na[0].message.contains("can never fail"));
    }

    #[test]
    fn record_suppression_with_why_exempts() {
        let f = scan_src("def f(x):\n    return {\"a\": 1, \"b\": x}  # lucidlint: ignore record-shape genuine map\n");
        assert!(!f.iter().any(|x| x.kind == "record-shape"));
    }

    #[test]
    fn magic_number_is_a_warn_never_fail() {
        let f = scan_src("def alpha(a):\n    return a * 3\n");
        let magic: Vec<&Finding> = f.iter().filter(|x| x.kind == "magic-number").collect();
        assert_eq!(magic.len(), 1);
        assert_eq!(magic[0].severity, "warn");
        assert_eq!(magic[0].line, 2);
    }

    // ------------------------------------------------- duplicate (Dice)
    /// Corpus-level scan: names map to sources; returns merged findings.
    fn scan_corpus(files: &[(&str, &str)]) -> Vec<Finding> {
        let mut skeletons = Vec::new();
        let mut definitions = Vec::new();
        let mut prod_refs = HashSet::new();
        let mut test_refs = HashSet::new();
        let mut strings = Vec::new();
        let mut all = Vec::new();
        let mut supps_by_rel: std::collections::HashMap<String, common::Suppressions> =
            std::collections::HashMap::new();
        let root = "repo";
        for (name, src) in files {
            let mut scan = scan_source(src, name);
            scan.file_name = name.to_string();
            supps_by_rel.insert(name.to_string(), std::mem::take(&mut scan.supps));
            all.extend(scan.findings);
            let rel = name.to_string();
            let is_test = is_test_path(&rel);
            for s in &scan.skeletons {
                skeletons.push(SkeletonFn {
                    rel: rel.clone(),
                    name: s.name.clone(),
                    line: s.line,
                    skeleton: s.skeleton.clone(),
                });
            }
            for (fn_name, line) in &scan.defs {
                definitions.push((rel.clone(), fn_name.clone(), *line));
            }
            if is_test {
                test_refs.extend(scan.refs.iter().cloned());
            } else {
                prod_refs.extend(scan.refs.iter().cloned());
                strings.extend(scan.strings.iter().cloned());
            }
            prod_refs.extend(scan.decorated.iter().cloned());
        }
        all.extend(duplicate_findings(&skeletons));
        let repo_wide_unused = unused_findings(&definitions, &prod_refs, &test_refs, &strings);
        reconcile_repo_wide(&mut all, repo_wide_unused, &supps_by_rel);
        // mirror the production finalize: repo-wide findings honor per-file
        // suppressions too (family-aware, widened window), and a suppression
        // the retain uses is not stale
        let mut used_ln: std::collections::HashSet<(usize, String)> = std::collections::HashSet::new();
        let mut used_fl: std::collections::HashSet<String> = std::collections::HashSet::new();
        let _ = root;
        let retained: Vec<Finding> = all
            .into_iter()
            .filter(|f| match supps_by_rel.get(&f.file) {
                Some(supps) => {
                    let kept = common::filter_repo_wide(vec![f.clone()], supps, &mut used_ln, &mut used_fl);
                    !kept.is_empty() // false = suppressed (recorded) + dropped; true = keep
                }
                None => true,
            })
            .collect();
        let filtered: Vec<Finding> = retained
            .into_iter()
            .filter(|f| {
                if f.kind != "stale-suppression" {
                    return true;
                }
                let sig = sig_of_stale(&f.message);
                !used_ln.contains(&(f.line, sig.clone())) && !used_fl.contains(&sig)
            })
            .collect();
        all = filtered;
        all
    }

    #[test]
    fn duplicate_near_identical_functions_are_found() {
        // same shape with renamed identifiers — Dice >= 0.9
        let f = scan_src(
            "def price_a(items):\n    total = 0\n    for it in items:\n        total += it.cost\n    return total\n\ndef price_b(parts):\n    total = 0\n    for p in parts:\n        total += p.cost\n    return total\n",
        );
        let d: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate").collect();
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn duplicate_different_shapes_are_not_found() {
        let f = scan_src(
            "def alpha(a):\n    return a * 3\n\ndef beta(b):\n    if b:\n        return [x for x in b if x]\n    return []\n",
        );
        assert!(!f.iter().any(|x| x.kind == "duplicate"));
    }

    #[test]
    fn test_with_unittest_assert_passes() {
        // unittest-style assertions count: self.assertEqual/assertTrue/...
        let ok = scan_src_test("def test_x(self):\n    self.assertEqual(1, 1)\n    self.assertTrue(True)\n");
        assert!(!ok.iter().any(|x| x.kind == "no-assert-test"));
        let ok2 = scan_src_test("def test_x(self):\n    with self.assertRaises(ValueError):\n        int('x')\n");
        assert!(!ok2.iter().any(|x| x.kind == "no-assert-test"));
    }

    #[test]
    fn async_def_record_shape_is_found() {
        // async def parses as FunctionDef with is_async at the ruff pin —
        // the signature pass must still see it (review: async gap)
        let f = scan_src("from typing import Any\nasync def fetch(url: str) -> dict[str, Any]:\n    return {}\n");
        assert!(f.iter().any(|x| x.kind == "record-shape"));
    }

    #[test]
    fn repo_wide_duplicate_suppressed_by_comment() {
        // a why'd `ignore duplicate` on the finding's line suppresses the
        // repo-wide duplicate AND is not itself reported stale
        let f = scan_corpus(&[
            ("one.py", "def alpha(a, b):\n    x = a + b\n    if x > 10:\n        return x * 2\n    return x\n"),
            ("two.py", "def alpha(a, b):  # lucidlint: ignore duplicate known pair — intentional scaffold\n    x = a + b\n    if x > 10:\n        return x * 2\n    return x\n"),
        ]);
        assert!(!f.iter().any(|x| x.kind == "duplicate"), "duplicate not suppressed");
        assert!(
            !f.iter().any(|x| x.kind == "stale-suppression"),
            "used suppression flagged stale"
        );
    }

    #[test]
    fn same_variable_name_different_types_are_distinct_families() {
        // two functions both dispatch a variable named x but over different
        // types — distinct element families, so no latent-visitor (the old
        // value-name key merged them into one family)
        let src = "class A:\n    pass\n\nclass B:\n    pass\n\nclass C:\n    pass\n\nclass D:\n    pass\n\ndef op1(x):\n    if isinstance(x, A):\n        return 1\n    elif isinstance(x, B):\n        return 2\n    return 0\n\ndef op2(x):\n    if isinstance(x, C):\n        return 3\n    elif isinstance(x, D):\n        return 4\n    return 0\n";
        let f = scan_src(src);
        assert!(!f.iter().any(|x| x.kind == "latent-visitor"));
    }

    #[test]
    fn why_less_comma_suppression_reports_each_signal() {
        // `ignore sig1,sig2` without a why: BOTH signals need a reason
        let f = scan_src("def f():\n    return 1  # lucidlint: ignore magic-number,noop-statement\n");
        let s: Vec<&Finding> = f.iter().filter(|x| x.kind == "suppression").collect();
        assert_eq!(s.len(), 2, "each signal's missing why must be reported");
    }

    #[test]
    fn duplicate_dice_contract_no_set_hash_shortcut() {
        // The Dice contract: >= 0.9 by MULTISET bigrams. Equal unique bigram
        // sets with different multiplicities hash equal under a set hash but
        // score Dice < 0.9 — the set-hash shortcut reported these as 100%
        // similar (2026-08-17 review-log). There is no hash shortcut; the
        // pair must NOT be flagged.
        use common::dice_similarity;
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // [a,b,a,b] has multiset bigrams (a,b),(b,a),(a,b) — the (a,b) repeats
        let rep = s(&["a", "b", "a", "b"]);
        // [a,b,a] has (a,b),(b,a) — same unique set, one fewer (a,b)
        let uniq = s(&["a", "b", "a"]);
        let sim = dice_similarity(&rep, &uniq);
        // 2*2 / (3+2) = 0.8 < 0.9 — NOT a duplicate despite equal unique sets
        assert!(sim < 0.9, "dice must count bigram multiplicities: {sim}");
        // sanity: the exact multiset is 1.0
        assert_eq!(dice_similarity(&rep, &s(&["a", "b", "a", "b"])), 1.0);
    }

    #[test]
    fn duplicate_skips_init_and_accessors() {
        let f = scan_src(
            "class Box:\n    def __init__(self, x):\n        self.x = x\n\n    def get(self):\n        return self.x\n",
        );
        assert!(!f.iter().any(|x| x.kind == "duplicate"));
    }

    #[test]
    fn duplicate_bfs_order_flags_the_later_class_method() {
        // module-level fn comes BEFORE class methods in ast.walk BFS — the
        // finding lands on the class method even though it has the lower line
        let f = scan_src(
            "def span(grid, row, c0, c1):\n    lat_min = max(grid.a, grid.a + row * grid.d)\n    lat_max = min(grid.b, grid.a + (row + 1) * grid.d)\n    lon_min = max(grid.c, grid.c + c0 * grid.e)\n    lon_max = min(grid.d, grid.c + (c1 + 1) * grid.e)\n    return Rect(lat_min, lat_max, lon_min, lon_max)\n\nclass Grid:\n    def cell_rect(self, r, c):\n        lat_min = max(self.bbox.a, self.bbox.a + r * self.lat_deg)\n        lat_max = min(self.bbox.b, self.bbox.a + (r + 1) * self.lat_deg)\n        lon_min = max(self.bbox.c, self.bbox.c + c * self.lon_deg)\n        lon_max = min(self.bbox.d, self.bbox.c + (c + 1) * self.lon_deg)\n        return Rect(lat_min, lat_max, lon_min, lon_max)\n",
        );
        let d: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate").collect();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].function, "cell_rect");
        assert_eq!(d[0].line, 9);
    }

    #[test]
    fn dice_partial_similarity_below_threshold() {
        let a: Vec<String> = "A B C D E".split(' ').map(str::to_string).collect();
        let b: Vec<String> = "A B X Y Z".split(' ').map(str::to_string).collect();
        assert!(checks::dice_similarity(&a, &b) < 0.9);
        let same: Vec<String> = "A B C D E".split(' ').map(str::to_string).collect();
        assert_eq!(checks::dice_similarity(&a, &same), 1.0);
    }

    // ------------------------------------------------- unused
    #[test]
    fn unused_never_referenced_is_found() {
        let f = scan_src("def helper():\n    return 1\n");
        let u: Vec<&Finding> = f.iter().filter(|x| x.kind == "unused").collect();
        assert_eq!(u.len(), 1);
        assert!(u[0].message.contains("never referenced"));
    }

    #[test]
    fn unused_referenced_and_main_skip() {
        let f = scan_src("def main():\n    return helper()\n\ndef helper():\n    return 1\n");
        assert!(!f.iter().any(|x| x.kind == "unused"));
    }

    #[test]
    fn unused_string_dispatch_mention_skips() {
        let f = scan_src("COMMANDS = [\"import_places\"]\n\ndef import_places():\n    return 1\n");
        assert!(!f.iter().any(|x| x.kind == "unused"));
    }

    #[test]
    fn unused_decorated_is_referenced() {
        let f = scan_src("@app.route(\"/x\")\ndef import_places():\n    return 1\n");
        assert!(!f.iter().any(|x| x.kind == "unused"));
    }

    #[test]
    fn unused_test_only_is_conditional() {
        let f = scan_corpus(&[
            ("prod.py", "def seam():\n    return 1\n"),
            ("tests/test_prod.py", "def test_seam():\n    assert seam()\n"),
        ]);
        let u: Vec<&Finding> = f.iter().filter(|x| x.kind == "unused").collect();
        assert_eq!(u.len(), 1);
        assert!(u[0].message.contains("referenced only from tests"));
    }

    #[test]
    fn unused_in_module_helper_used_later_is_not_flagged() {
        // B2 (review log §1.2): a helper defined and CALLED within the same
        // module is referenced — never a false "unused". (The v0.1.0 bundle
        // counted only cross-file refs and flagged in-module helpers.)
        let f = scan_corpus(&[(
            "m.py",
            "def main():\n    return f()\n\ndef _slug(s):\n    return s\n\ndef f():\n    return _slug('x')\n",
        )]);
        assert!(!f.iter().any(|x| x.kind == "unused"), "{:?}", f);
    }

    #[test]
    fn unused_ignore_comment_is_not_self_defeating() {
        // B3 (review log §5.1): `# lucidlint: ignore unused <why>` must
        // suppress the unused finding AND not be reported stale — a
        // suppression that names a real finding is used, never dead weight.
        let f = scan_corpus(&[(
            "m.py",
            "# lucidlint: ignore unused deliberate helper\n\ndef _helper():\n    return 1\n",
        )]);
        assert!(!f.iter().any(|x| x.kind == "unused"), "{:?}", f);
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{:?}", f);
    }

    #[test]
    fn positional_literals_flagged() {
        let f = scan_src("def f():\n    pass\ndef g():\n    set_limits(10, 20)\n");
        assert!(f
            .iter()
            .any(|x| x.kind == "positional-literals" && x.severity == "warn"));
        let ok = scan_src("def f():\n    pass\ndef g():\n    set_limits(max=10, min=20)\n");
        assert!(!ok.iter().any(|x| x.kind == "positional-literals"));
        let ok2 = scan_src("def g():\n    range(1, 10)\n");
        assert!(!ok2.iter().any(|x| x.kind == "positional-literals")); // builtin exempt
    }

    #[test]
    fn detached_method_flagged() {
        let f = scan_src("class A:\n    def m(self, x):\n        return x + 1\n");
        assert!(f.iter().any(|x| x.kind == "detached-method"));
        let ok = scan_src("class A:\n    def m(self, x):\n        return self.x + x\n");
        assert!(!ok.iter().any(|x| x.kind == "detached-method"));
        let cm = scan_src("class A:\n    @classmethod\n    def m(cls, x):\n        return x\n");
        assert!(!cm.iter().any(|x| x.kind == "detached-method")); // classmethod exempt
        let st = scan_src("class A:\n    @staticmethod\n    def m(x):\n        return x\n");
        assert!(!st.iter().any(|x| x.kind == "detached-method"));
    }

    #[test]
    fn ignore_duplicate_suppresses_repo_wide_pair_without_stale() {
        // an `ignore duplicate <why>` comment must exempt the repo-wide
        // duplicate finding AND not be flagged stale (B3 wired for every
        // repo-wide family, not just unused — the per-file pass runs before
        // the duplicate pass)
        let pair = |suffix: &str| {
            format!(
                "def span{a}(grid, row, c0, c1):\n    lat_min = max(grid.a, grid.a + row * grid.d)\n    lat_max = min(grid.b, grid.a + (row + 1) * grid.d)\n    lon_min = max(grid.c, grid.c + c0 * grid.e)\n    lon_max = min(grid.d, grid.c + (c1 + 1) * grid.e)\n    return Rect(lat_min, lat_max, lon_min, lon_max)\n",
                a = suffix
            )
        };
        let src = format!(
            "class Rect:\n    def __init__(self, a, b, c, d):\n        self.a = a\n        self.b = b\n        self.c = c\n        self.d = d\n\n{pair1}{pair2}",
            pair1 = pair("1"),
            pair2 = "# lucidlint: ignore duplicate known copy-paste grid math\n".to_owned() + &pair("2"),
        );
        let f = scan_corpus(&[("prod_mod.py", &src)]);
        assert!(
            !f.iter().any(|x| x.kind == "duplicate"),
            "the ignore must suppress the duplicate"
        );
        assert!(
            !f.iter().any(|x| x.kind == "stale-suppression"),
            "the used suppression must not be stale"
        );
    }

    #[test]
    fn complexity_suppression_three_lines_above_not_stale() {
        // B7: a decorator line may intervene between the suppression comment
        // and the def. With a 3-line window the comment directly above the
        // decorator still suppresses the cc finding, and the used suppression
        // is not flagged stale (the cc retain records the SAME window).
        let mut body = String::new();
        for i in 0..16 {
            body.push_str(&format!("    if c{i}:\n        x{i} = {i}\n"));
        }
        let src = format!(
            "# lucidlint: ignore complexity calibrated\n@deco\ndef f({c0}, {c1}, {c2}, {c3}, {c4}, {c5}, {c6}, {c7}, {c8}, {c9}, {c10}, {c11}, {c12}, {c13}, {c14}, {c15}):\n{body}    return 0\n",
            c0 = "a", c1 = "b", c2 = "c", c3 = "d", c4 = "e", c5 = "f", c6 = "g", c7 = "h",
            c8 = "i", c9 = "j", c10 = "k", c11 = "l", c12 = "m", c13 = "n", c14 = "o", c15 = "p",
        );
        let f = scan_src(&src);
        assert!(
            !f.iter().any(|x| x.kind == "complexity"),
            "the windowed suppression must hold"
        );
        assert!(
            !f.iter().any(|x| x.kind == "stale-suppression"),
            "a used suppression is not stale"
        );
    }

    #[test]
    fn comma_signal_suppresses_both_families() {
        // one comment, comma-separated signals — the only shape that fits the
        // line/line-1 window for two families on one def
        let src = "class X:\n    # lucidlint: ignore long-param-list,detached-method override signature\n    def m(self, a, b, c, d, e, f):\n        return 1\n";
        let f = scan_src(src);
        assert!(!f.iter().any(|x| x.kind == "long-param-list"));
        assert!(!f.iter().any(|x| x.kind == "detached-method"));
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"));
    }

    #[test]
    fn guard_clauses_arrow_code_detected() {
        let f = scan_src(
            "def f(a, b, c):\n    if a:\n        if b:\n            if c:\n                return 1\n    return 0\n",
        );
        assert!(f.iter().any(|x| x.kind == "guard-clauses"));
        let ok = scan_src("def f(a, b):\n    if a:\n        if b:\n            return 1\n    return 0\n");
        assert!(!ok.iter().any(|x| x.kind == "guard-clauses")); // 2 levels is fine
    }

    #[test]
    fn conditional_polymorphism_dispatch_detected() {
        let src = "def f(x):\n    if x == 1:\n        return 'a'\n    elif x == 2:\n        return 'b'\n    elif x == 3:\n        return 'c'\n    elif x == 4:\n        return 'd'\n    return '?'\n";
        let f = scan_src(src);
        assert!(f.iter().any(|x| x.kind == "conditional-polymorphism"));
        let ok = scan_src(
            "def f(x, y):\n    if x == 1:\n        return 'a'\n    elif y == 2:\n        return 'b'\n    return '?'\n",
        );
        assert!(!ok.iter().any(|x| x.kind == "conditional-polymorphism")); // mixed keys
    }

    #[test]
    fn special_case_skips_fail_fast_raise_guards() {
        // review log §2.6: `person is None -> raise KeyError` guards are NOT
        // special-case candidates — the absence is an error, no object can
        // replace it (the fix suggestion would mask the error)
        let f = scan_src(
            "def g(person):\n    if person is None:\n        raise KeyError('x')\n    if person is None:\n        raise KeyError('y')\n    if person is None:\n        raise KeyError('z')\n    return person\n",
        );
        assert!(!f.iter().any(|x| x.kind == "special-case"), "{:?}", f);
        // control: repeated VALUE-style null handling still fires
        let ok = scan_src(
            "def f(a):\n    if a is None:\n        return 1\n    if a is None:\n        return 2\n    if a is None:\n        return 3\n    return 0\n",
        );
        assert!(ok.iter().any(|x| x.kind == "special-case"));
        // an ERROR-return guard (mine is None -> 401 JSONResponse) is a
        // boundary, not a special case — a NullObject would hide the 401
        let guard = scan_src(
            "def g(mine):\n    if mine is None:\n        return JSONResponse({'error': 'x'}, status_code=401)\n    if mine is None:\n        return JSONResponse({'error': 'y'}, status_code=401)\n    if mine is None:\n        return JSONResponse({'error': 'z'}, status_code=401)\n    return mine\n",
        );
        assert!(!guard.iter().any(|x| x.kind == "special-case"), "{:?}", guard);
    }

    #[test]
    fn long_param_list_skips_trivial_framework_stub() {
        // review: urllib's redirect_request override — 6 protocol params, a
        // one-statement stub; a parameter object would BREAK the override.
        // A trivial stub is a protocol placeholder, not a param-list smell.
        let f = scan_src(
            "class _NoRedirect:\n    def redirect_request(self, req, fp, code, msg, headers, newurl):\n        return None\n",
        );
        assert!(!f.iter().any(|x| x.kind == "long-param-list"), "{:?}", f);
        // control: a real 6-param fn with a body still fires
        let ok = scan_src("def f(a, b, c, d, e, g):\n    return a + b + c + d + e + g\n");
        assert!(ok.iter().any(|x| x.kind == "long-param-list"), "{:?}", ok);
    }

    #[test]
    fn python_fn_shape_classifies_dispatch_and_rules() {
        // the shape classifier behind the complexity message: a chain of
        // ifs over the same selector is a dispatch chain; a battery of ifs
        // appending to the same list is a rules battery
        let dispatch = "def route(sel):\n    if sel == \"a\":\n        return 1\n    if sel == \"b\":\n        return 2\n    if sel == \"c\":\n        return 3\n    return -1\n";
        let (shape, detail) = python_fn_shape(&first_py_fn_body(dispatch));
        assert_eq!((shape, detail.as_str()), ("dispatch", "sel"));
        let rules = "def check(a):\n    out = []\n    if a.get(1):\n        out.append('x')\n    if a.get(2):\n        out.append('y')\n    if a.get(3):\n        out.append('z')\n    return out\n";
        let (shape, detail) = python_fn_shape(&first_py_fn_body(rules));
        assert_eq!((shape, detail.as_str()), ("rules", "out"));
        let plain = "def f(a):\n    x = a + 1\n    return x\n";
        let (shape, _) = python_fn_shape(&first_py_fn_body(plain));
        assert_eq!(shape, "plain");
        // the OFFER equals the FIX: a battery the rule-table fix cannot
        // apply to (a check with a nested if — not a single append) is NOT
        // routed to rule-table; it is "plain" so the message offers
        // extract-method instead (no false directive)
        let unfixable = "def check(a):\n    out = []\n    if a.get(1):\n        if a.get(2):\n            out.append('x')\n    if a.get(3):\n        out.append('y')\n    if a.get(4):\n        out.append('z')\n    return out\n";
        let (shape, _) = python_fn_shape(&first_py_fn_body(unfixable));
        assert_eq!(shape, "plain", "an unhoistable battery must not be offered rule-table");
        // a dispatch with an elif is not the fixable shape
        let unfixable_d = "def route(sel):\n    if sel == \"a\":\n        return 1\n    elif sel == \"b\":\n        return 2\n    if sel == \"c\":\n        return 3\n    return -1\n";
        let (shape, _) = python_fn_shape(&first_py_fn_body(unfixable_d));
        assert_eq!(shape, "plain", "an elif dispatch must not be offered dispatch-registry");
    }

    #[test]
    fn special_case_repeated_none_checks() {
        let src = "def f(a):\n    if a is None:\n        return 1\n    if a is None:\n        return 2\n    if a is None:\n        return 3\n    return 0\n";
        let f = scan_src(src);
        assert!(f.iter().any(|x| x.kind == "special-case"));
        let ok = scan_src("def f(a):\n    if a is None:\n        return 1\n    return 0\n");
        assert!(!ok.iter().any(|x| x.kind == "special-case"));
    }

    #[test]
    fn middle_man_delegation_detected() {
        let f = scan_src("class A:\n    def go(self, x):\n        return self.inner.go(x)\n");
        assert!(f.iter().any(|x| x.kind == "middle-man"));
        let ok = scan_src("class A:\n    def go(self, x):\n        self.count += 1\n        return self.inner.go(x)\n");
        assert!(!ok.iter().any(|x| x.kind == "middle-man"));
    }

    #[test]
    fn unused_setter_detected() {
        let f = scan_src("class A:\n    def set_x(self, v):\n        self.x = v\n");
        assert!(f.iter().any(|x| x.kind == "unused-setter"));
        let ok = scan_src(
            "class A:\n    def set_x(self, v):\n        self.x = v\n    def get(self):\n        return self.set_x(1)\n",
        );
        assert!(!ok.iter().any(|x| x.kind == "unused-setter"));
    }

    #[test]
    fn loop_pipeline_detected() {
        let f = scan_src("def f(xs):\n    out = []\n    for x in xs:\n        out.append(x)\n    return out\n");
        assert!(f.iter().any(|x| x.kind == "loop-pipeline"));
        let ok = scan_src("def f(xs):\n    total = 0\n    for x in xs:\n        total += x\n    return total\n");
        assert!(!ok.iter().any(|x| x.kind == "loop-pipeline"));
    }

    #[test]
    fn latent_visitor_detected_and_claims_chains() {
        // two operations over the same element family — the visitor shape
        let src = "class A:\n    pass\n\nclass B:\n    pass\n\ndef op1(x):\n    if isinstance(x, A):\n        return 1\n    elif isinstance(x, B):\n        return 2\n    return 0\n\ndef op2(x):\n    if isinstance(x, A):\n        return 3\n    elif isinstance(x, B):\n        return 4\n    return 0\n";
        let f = scan_src(src);
        assert!(f.iter().any(|x| x.kind == "latent-visitor"), "expected latent-visitor");
        // the claimed chains must NOT also be polymorphism — one ruling per chain
        assert!(
            !f.iter().any(|x| x.kind == "conditional-polymorphism"),
            "claimed chains must be exempt from conditional-polymorphism"
        );
    }

    #[test]
    fn single_dispatch_stays_polymorphism() {
        // ONE operation over the family (4-arm chain) — polymorphic methods,
        // not a visitor (a single 2-arm type check is idiomatic, no finding)
        let src = "class A:\n    pass\n\nclass B:\n    pass\n\nclass C:\n    pass\n\nclass D:\n    pass\n\ndef op1(x):\n    if isinstance(x, A):\n        return 1\n    elif isinstance(x, B):\n        return 2\n    elif isinstance(x, C):\n        return 3\n    elif isinstance(x, D):\n        return 4\n    return 0\n";
        let f = scan_src(src);
        assert!(f.iter().any(|x| x.kind == "conditional-polymorphism"));
        assert!(!f.iter().any(|x| x.kind == "latent-visitor"));
    }

    #[test]
    fn value_dispatch_is_not_a_visitor() {
        // x == 1/2/3/4 is value dispatch, not a type-tag visitor
        let src = "def f(x):\n    if x == 1:\n        return 'a'\n    elif x == 2:\n        return 'b'\n    elif x == 3:\n        return 'c'\n    elif x == 4:\n        return 'd'\n    return '?'\n";
        let f = scan_src(src);
        assert!(f.iter().any(|x| x.kind == "conditional-polymorphism"));
        assert!(!f.iter().any(|x| x.kind == "latent-visitor"));
    }

    #[test]
    fn implemented_visitor_is_not_flagged() {
        // the real visitor: accept on the elements + visit_* on the visitor —
        // no dispatch chain remains, so nothing new fires (no thrash)
        let src = "class A:\n    def accept(self, v):\n        return v.visit_a(self)\n\nclass B:\n    def accept(self, v):\n        return v.visit_b(self)\n\nclass Visitor:\n    def visit_a(self, e):\n        return 1\n    def visit_b(self, e):\n        return 2\n";
        let f = scan_src(src);
        assert!(!f.iter().any(|x| x.kind == "latent-visitor"));
        assert!(!f.iter().any(|x| x.kind == "conditional-polymorphism"));
    }

    #[test]
    fn registry_is_complete() {
        // every emitted kind has a final_kind arm OR is deliberately standard
        for &k in crate::FAMILY_KINDS {
            let display = crate::final_kind(k);
            assert!(
                display != "standard" || crate::STANDARD_KINDS.contains(&k),
                "kind '{k}' has no final_kind arm and is not in STANDARD_KINDS — register it"
            );
        }
        // the standard list is a strict subset of the family list
        for &k in crate::STANDARD_KINDS {
            assert!(
                crate::FAMILY_KINDS.contains(&k),
                "standard kind '{k}' is not in FAMILY_KINDS"
            );
        }
        // no kind is both named and standard
        let mut named = Vec::new();
        for &k in crate::FAMILY_KINDS {
            if crate::final_kind(k) != "standard" {
                named.push(k);
            }
        }
        for &k in crate::STANDARD_KINDS {
            assert!(
                !named.contains(&k),
                "kind '{k}' has a final_kind arm and is also STANDARD_KINDS — pick one"
            );
        }
    }
}

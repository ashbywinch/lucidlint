//! code-health-scan — the Rust scan core for the deterministic code-health gate.
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

use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
use ruff_python_ast::{AnyNodeRef, Expr, ModModule, Stmt};
use ruff_python_parser::{parse_module, Parsed};
use ruff_text_size::Ranged;
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;

mod checks;
mod graph_families;
mod lsp;
use checks::*;
use checks::Q;

/// One open function scope: its decision count and the names it returns
/// (for the except-swallow analysis).
pub struct FnScope {
    pub decisions: u32,
    pub returned: HashSet<String>,
}

#[derive(Serialize, Clone)]
struct Finding {
    file: String,
    line: usize,
    function: String,
    kind: String,
    severity: String,
    message: String,
}

/// One function's cyclomatic complexity — radon-equivalent counting.
#[derive(Serialize)]
struct FnCc {
    file: String,
    function: String,
    line: usize,
    cc: u32,
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
    /// function's returned names (for the except-swallow analysis).
    fn_stack: Vec<FnScope>,
    /// Class nesting: class bodies contribute no decisions (radon sub-visitor).
    in_class: u32,
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
    /// radon's visit_Assert never recurses: an assert counts 1 and its
    /// test/msg subtrees contribute no decisions. Bumped while walking one.
    suppress_decisions: u32,
}

fn line_of(source: &str, offset: ruff_text_size::TextSize) -> usize {
    1 + source[..offset.to_usize()].bytes().filter(|&b| b == b'\n').count()
}

enum ParentEntry {
    Expr(Expr),
    Stmt,
    Keyword,
}

fn expr_range(e: &Expr) -> ruff_text_size::TextRange {
    match e {
        Expr::BoolOp(x) => x.range(),
        Expr::Named(x) => x.range(),
        Expr::BinOp(x) => x.range(),
        Expr::UnaryOp(x) => x.range(),
        Expr::Lambda(x) => x.range(),
        Expr::If(x) => x.range(),
        Expr::Dict(x) => x.range(),
        Expr::Set(x) => x.range(),
        Expr::ListComp(x) => x.range(),
        Expr::SetComp(x) => x.range(),
        Expr::DictComp(x) => x.range(),
        Expr::Generator(x) => x.range(),
        Expr::Await(x) => x.range(),
        Expr::Yield(x) => x.range(),
        Expr::YieldFrom(x) => x.range(),
        Expr::Compare(x) => x.range(),
        Expr::Call(x) => x.range(),
        Expr::FString(x) => x.range(),
        Expr::TString(x) => x.range(),
        Expr::StringLiteral(x) => x.range(),
        Expr::BytesLiteral(x) => x.range(),
        Expr::NumberLiteral(x) => x.range(),
        Expr::BooleanLiteral(x) => x.range(),
        Expr::NoneLiteral(x) => x.range(),
        Expr::EllipsisLiteral(x) => x.range(),
        Expr::Attribute(x) => x.range(),
        Expr::Subscript(x) => x.range(),
        Expr::Starred(x) => x.range(),
        Expr::Name(x) => x.range(),
        Expr::List(x) => x.range(),
        Expr::Tuple(x) => x.range(),
        Expr::Slice(x) => x.range(),
        Expr::IpyEscapeCommand(x) => x.range(),
    }
}

impl<'a> SourceOrderVisitor<'a> for ScanState<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        // module scope = no open function scope and no class nesting (the
        // FunctionDef arm walks bodies manually, so parent_stack is not the
        // reliable signal here)
        let module_level = self.fn_stack.is_empty() && self.in_class == 0;
        match stmt {
            Stmt::FunctionDef(f) => {
                let module_level = self.fn_stack.is_empty() && self.in_class == 0;
                let was_fn = self.current_fn.take();
                self.current_fn =
                    Some((f.name.to_string(), line_of(self.source, f.range.start())));
                self.fn_stack.push(FnScope {
                    decisions: 0,
                    returned: returned_names(&f.body),
                });
                self.unreachable_check(&f.body);
                shadow_findings(self, stmt);
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
                let scope = self.fn_stack.pop().unwrap();
                let cc = scope.decisions + 1;
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
                let span = (line_of(self.source, f.range().end())
                    - line_of(self.source, f.range().start())) as u32;
                // the Python gate reads cc from radon's fn_map, which only
                // holds module-level functions — methods/nested get cc = 0
                let gate_cc = if module_level { cc } else { 0 };
                closure_findings(self, stmt, gate_cc, span);
                if module_level {
                    // the def line (name range), not the decorator — radon's
                    // fn.lineno; the parity test's decorated-line offset
                    // normalization existed because of this difference
                    self.cc.push(FnCc {
                        file: self.file.to_string(),
                        function: f.name.to_string(),
                        line: def_line,
                        cc,
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
            Stmt::Global(_) => global_state_findings(self, stmt, false),
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
        // cyclomatic decision points (radon-equivalent CCN) — only inside a
        // function scope, outside class bodies
        self.parent_stack.push(ParentEntry::Stmt);
        if !self.fn_stack.is_empty() && self.in_class == 0 {
            match stmt {
                Stmt::If(i) => {
                    // radon: if + each elif counts, the trailing else does NOT
                    let elifs = i
                        .elif_else_clauses
                        .iter()
                        .filter(|c| c.test.is_some())
                        .count() as u32;
                    self.fn_stack.last_mut().unwrap().decisions += 1 + elifs;
                }
                Stmt::For(f) => {
                    self.fn_stack.last_mut().unwrap().decisions += 1 + (!f.orelse.is_empty()) as u32
                }
                Stmt::While(w) => {
                    self.fn_stack.last_mut().unwrap().decisions += 1 + (!w.orelse.is_empty()) as u32
                }
                Stmt::Try(t) => {
                    self.fn_stack.last_mut().unwrap().decisions +=
                        t.handlers.len() as u32 + (!t.orelse.is_empty()) as u32
                }
                Stmt::Assert(_) => self.fn_stack.last_mut().unwrap().decisions += 1,
                Stmt::Match(m) => {
                    for case in &m.cases {
                        let wildcard = matches!(
                            &case.pattern,
                            ruff_python_ast::Pattern::MatchAs(p)
                                if p.pattern.is_none() && p.name.is_none()
                        );
                        if !wildcard {
                            self.fn_stack.last_mut().unwrap().decisions += 1;
                        }
                    }
                }
                _ => {}
            }
        }
        // radon's visit_Assert does not recurse — the assert's subtrees
        // contribute no decisions (verified on 6.0.1)
        if matches!(stmt, Stmt::Assert(_)) {
            self.suppress_decisions += 1;
        }
        walk_stmt(self, stmt);
        if matches!(stmt, Stmt::Assert(_)) {
            self.suppress_decisions -= 1;
        }
        self.parent_stack.pop();
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(n) => {
                self.refs.insert(n.id.to_string());
            }
            Expr::StringLiteral(s) => {
                if !self.is_test {
                    self.strings.push(s.value.to_str().to_string());
                }
            }
            _ => {}
        }
        // Calls are walked manually so keyword values get a Keyword parent —
        // the generic walk would hand them the Call as parent, and the magic
        // position rule must match Python's (status_code=403 is NOT a finding).
        if let Expr::Call(call) = expr {
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
        if let Some(scope) = self.fn_stack.last_mut() {
            let slot = &mut scope.decisions;
            match expr {
                Expr::NumberLiteral(n) => self.magic_check(n),
                _ if self.suppress_decisions > 0 => {}
                _ => {
                    match expr {
                        Expr::BoolOp(b) => *slot += b.values.len().saturating_sub(1) as u32,
                        Expr::If(_) => *slot += 1,
                        // radon's lambda: +0, but the body IS walked (verified on 6.0.1)
                        // radon counts EACH generator clause (for x in ...) plus each
                        // if — a two-for comprehension is +2, not +1 (verified)
                        Expr::ListComp(c) => {
                            *slot += c.generators.len() as u32
                                + c.generators.iter().map(|g| g.ifs.len() as u32).sum::<u32>()
                        }
                        Expr::SetComp(c) => {
                            *slot += c.generators.len() as u32
                                + c.generators.iter().map(|g| g.ifs.len() as u32).sum::<u32>()
                        }
                        Expr::DictComp(c) => {
                            *slot += c.generators.len() as u32
                                + c.generators.iter().map(|g| g.ifs.len() as u32).sum::<u32>()
                        }
                        Expr::Generator(c) => {
                            *slot += c.generators.len() as u32
                                + c.generators.iter().map(|g| g.ifs.len() as u32).sum::<u32>()
                        }
                        _ => {}
                    }
                }
            }
        }
        self.parent_stack.push(ParentEntry::Expr(expr.clone()));
        walk_expr(self, expr);
        self.parent_stack.pop();
    }
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
            message: format!("magic number {value} — name it as a constant"),
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
                message: "expression statement discards its value — dead statement".into(),
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
                            format!(
                                "{}.{}",
                                im.module.as_ref().map(|m| m.as_str()).unwrap_or(""),
                                name
                            )
                        };
                        self.findings.push(Finding {
                            file: self.file.to_string(),
                            line: stmt_line(self.source, stmt),
                            function: fn_name.clone(),
                            kind: "private-import".into(),
                            severity: "fail".into(),
                            message: format!(
                                "imports private symbol '{target}' — never import underscore symbols"
                            ),
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
                            message: format!(
                                "imports private path '{name}' — never import underscore symbols"
                            ),
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
                                "unreachable statement at line {line} — dead code is deleted"
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
        Stmt::FunctionDef(_) => return,
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
                if let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h {
                    out.push(&eh.body);
                    for s in &eh.body {
                        collect_stmt_lists(s, out);
                    }
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
}

fn is_test_path(name: &str) -> bool {
    name.contains("/test") || name.starts_with("test")
}

/// The full scan (CLI): per-file families + repo-wide collections.
fn scan_source(source: &str, name: &str) -> FileScan {
    scan_source_impl(source, name, true)
}

/// The LSP scan: per-file families only — the duplicate-candidate BFS and
/// its per-function skeleton walks are pure waste for a single buffer.
pub fn scan_source_lsp(source: &str, name: &str) -> FileScan {
    scan_source_impl(source, name, false)
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
    class_module_findings(&mut state, &body, name);
    vague_name_findings(&mut state, &body);
    strewing_findings(&mut state, &body);
    record_shape_findings(&mut state, &body, source);
    partition_findings(&mut state, &body, source);
    if state.is_test {
        // the rules that live in tests — scanned for alone or with --include-tests
        monkeypatch_findings(&mut state, &body, source);
        skipif_findings(&mut state, &body, source);
        fakefs_findings(&mut state, &body, source);
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
    let findings = apply_suppressions(state.findings, source, name, tokens);
    let mut all = type_ignore_findings(source, name, tokens);
    all.extend(findings);
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
    }
}

fn scan_file(path: &Path) -> FileScan {
    let source = std::fs::read_to_string(path).unwrap_or_default();
    let name = path.to_str().unwrap_or("<file>").to_string();
    let mut scan = scan_source(&source, &name);
    scan.file_name = name;
    scan
}

/// Longest common prefix of the passed paths — the repo root for a
/// full-repo run; the file itself in per-file mode.
///
/// The char-level prefix can stop mid-component (tests/ vs tools/ share a
/// trailing 't'), so it backs off to the last path separator — rels are
/// always clean directory boundaries.
fn repo_root(paths: &[String]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let mut prefix = paths[0].clone();
    for p in &paths[1..] {
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

fn rel_of(path: &str, root: &str) -> String {
    if let Some(rel) = path.strip_prefix(root).and_then(|r| r.strip_prefix('/')) {
        return rel.to_string();
    }
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--lsp") {
        // stdio JSON-RPC language server — in-process scans, no spawns
        lsp::run();
        return;
    }
    // flag parsing: --graph <json> --churn <json> --include-tests then files
    let mut graph_path: Option<String> = None;
    let mut churn_path: Option<String> = None;
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
            "--include-tests" => include_tests = true,
            _ => paths.push(args[i].clone()),
        }
        i += 1;
    }
    let root = repo_root(&paths);
    let mut scans = Vec::new();
    for path in &paths {
        scans.push(scan_file(Path::new(path)));
    }
    // repo-wide families: duplicate (Dice) + unused (reference scan)
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
    for scan in scans {
        all_findings.extend(scan.findings);
        all_cc.extend(scan.cc);
        total_errors += scan.errors;
    }
    all_findings.extend(duplicate_findings(&skeletons));
    all_findings.extend(unused_findings(&definitions, &prod_refs, &test_refs, &strings));
    // repo-wide families from the graph contract + git churn (orchestrator
    // gathers both; the findings compute here)
    let contract = graph_path
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<graph_families::GraphContract>(&s).ok());
    if let Some(c) = &contract {
        let repo_root = Path::new(&root);
        let mut max_cc_by_file: std::collections::HashMap<String, (usize, usize, String)> =
            std::collections::HashMap::new();
        let mut cc_by_file: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for e in &all_cc {
            let rel = rel_of(&e.file, &root);
            *cc_by_file.entry(rel.clone()).or_default() = (*cc_by_file.get(&rel).unwrap_or(&0)).max(e.cc);
            let entry = max_cc_by_file.entry(rel).or_insert((e.line, e.cc as usize, e.function.clone()));
            if (e.cc as usize) > entry.1 {
                *entry = (e.line, e.cc as usize, e.function.clone());
            }
        }
        all_findings.extend(graph_families::large_function_findings(
            repo_root, c, 120, include_tests,
        ));
        all_findings.extend(graph_families::hub_file_findings(
            repo_root, c, 150, include_tests, &max_cc_by_file,
        ));
        all_findings.extend(graph_families::high_risk_findings(repo_root, c, 0.8, include_tests));
        all_findings.extend(graph_families::cycle_findings(repo_root, c));
        let rels: Vec<String> = paths.iter().map(|p| rel_of(p, &root)).collect();
        all_findings.extend(graph_families::layer_mix_findings(repo_root, c, &rels));
        all_findings.extend(graph_families::folder_mix_findings(repo_root, c));
    }
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
                    &churn, &cc_any, 0.1, 15, &std::collections::HashMap::new(),
                ));
            }
        }
    }
    let out = serde_json::json!({
        "files": paths.len(),
        "parse_errors": total_errors,
        "findings": all_findings,
        "cc": all_cc,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_src(src: &str) -> Vec<Finding> {
        scan_corpus(&[("prod_mod.py", src)])
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
        let src = "def f(a):\n    if a:\n        return 1\n    elif b:\n        return 2\n    else:\n        return 3\n";
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
        let f = scan_src("def f(a):\n    table = {10: 'x', 20: 'y'}\n    if a > 1:\n        return table[a]\n    return 0\n");
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
        let f = scan_src("def f():\n    try:\n        g()\n    except ValueError:  # code-health: ignore except this is safe, logged\n        log('x')\n");
        assert!(!f.iter().any(|x| x.kind == "except"));
    }

    #[test]
    fn suppression_without_why_is_a_finding() {
        let f = scan_src("def f():\n    try:\n        g()\n    except ValueError:  # code-health: ignore except\n        log('x')\n");
        assert!(f.iter().any(|x| x.kind == "suppression"));
        assert!(f.iter().any(|x| x.kind == "except")); // not actually exempted
    }

    #[test]
    fn suppression_on_line_above_exempts() {
        // the comment must sit on the finding's line or line-1 — the except
        // handler is line 5, so line 4 (the try) is the line-1 position
        let f = scan_src("def f():\n    try:\n        g()\n    # code-health: ignore except deliberate skip\n    except ValueError:\n        log('x')\n");
        assert!(!f.iter().any(|x| x.kind == "except"));
    }

    #[test]
    fn ignore_file_exempts() {
        let src = "# code-health: ignore-file class-module helper inside a CLI utility\nclass Helper:\n    def run(self):\n        return 1\n";
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

    // ------------------------------------------------------------- except family
    #[test]
    fn bare_and_log_only_swallows() {
        let f = scan_src("def f():\n    try:\n        g()\n    except:\n        pass\n    try:\n        h()\n    except ValueError:\n        log('x')\n");
        assert_eq!(f.iter().filter(|x| x.kind == "except").count(), 2);
    }

    #[test]
    fn surfaced_return_is_not_a_swallow() {
        let f = scan_src("def f():\n    try:\n        g()\n    except ValueError:\n        return 'failed'\n");
        assert!(!f.iter().any(|x| x.kind == "except"));
    }

    #[test]
    fn accumulator_surfacing_via_returned_name() {
        let f = scan_src("def validate(rows):\n    issues = []\n    for s in rows:\n        try:\n            parse(s)\n        except ValueError as e:\n            issues.append(str(e))\n    return issues\n");
        assert!(!f.iter().any(|x| x.kind == "except"));
    }

    #[test]
    fn sys_exit_surfaces() {
        let f = scan_src("import sys\ndef f():\n    try:\n        data = parse()\n    except ValueError:\n        sys.stderr.write('bad')\n        sys.exit(2)\n    return data\n");
        assert!(!f.iter().any(|x| x.kind == "except"));
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
        let closures: Vec<(usize, &str)> = f.iter()
            .filter(|x| x.kind == "closures")
            .map(|x| (x.line, x.message.as_str()))
            .collect();
        assert_eq!(closures.len(), 1, "expected one closure, got {closures:?}");
    }

    #[test]
    fn subscript_store_on_returned_name_surfaces() {
        let f = scan_src("def to_json_value(self):\n    result = {}\n    try:\n        g()\n    except Exception:\n        logger.exception('x')\n        result['value'] = None\n    return result\n");
        assert!(
            !f.iter().any(|x| x.kind == "except"),
            "expected no swallow, got {:?}",
            f.iter().map(|x| (x.kind.as_str(), x.line)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn broad_except_is_a_warn() {
        let f = scan_src("def f():\n    try:\n        g()\n    except Exception as e:\n        log(e)\n        return fallback\n");
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
        assert!(f.iter().any(|x| x.kind == "closures"), "expected closures, got {:?}", kinds(&f));
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

    // ------------------------------------------------- suppression scope
    #[test]
    fn suppression_scoped_to_its_line() {
        // an explained ignore on one except does not exempt a second except
        let f = scan_src("def f():\n    try:\n        g()\n    except ValueError:  # code-health: ignore except this one is safe, logged\n        log('a')\n    try:\n        h()\n    except ValueError:\n        log('b')\n");
        let exc: Vec<&Finding> = f.iter().filter(|x| x.kind == "except").collect();
        assert_eq!(exc.len(), 1);
        assert_eq!(exc[0].line, 8); // the second handler (line 8) is not exempted
    }

    #[test]
    fn suppression_wrong_signal_does_not_exempt() {
        let f = scan_src("def f():\n    try:\n        g()\n    except ValueError:  # code-health: ignore inline-import not the right signal\n        log('skipping')\n");
        assert!(f.iter().any(|x| x.kind == "except")); // still a swallow; an explained mis-scoped ignore emits no suppression finding
    }

    #[test]
    fn ignore_file_without_why_is_a_finding() {
        let src = "# code-health: ignore-file except\ndef f():\n    try:\n        g()\n    except ValueError:\n        log('x')\n";
        let f = scan_src(src);
        assert!(f.iter().any(|x| x.kind == "except")); // not exempted
        assert!(f.iter().any(|x| x.kind == "suppression"));
    }

    #[test]
    fn type_ignore_in_docstring_is_not_a_finding() {
        let f = scan_src("def f():\n    \"\"\"Never silence: type: ignore lives in real comments.\"\"\"\n    return 1\n");
        assert!(!f.iter().any(|x| x.kind == "type-ignore"));
    }

    // ------------------------------------------------- except edges
    #[test]
    fn except_with_raise_is_not_a_swallow() {
        let f = scan_src("def f():\n    try:\n        g()\n    except ValueError:\n        log('bad')\n        raise\n");
        assert!(!f.iter().any(|x| x.kind == "except"));
    }

    #[test]
    fn except_returning_empty_dict_is_not_a_swallow() {
        let f = scan_src("def f():\n    try:\n        g()\n    except ValueError:\n        return {}\n");
        assert!(!f.iter().any(|x| x.kind == "except"));
    }

    #[test]
    fn except_return_none_is_not_a_swallow() {
        let f = scan_src("def f():\n    try:\n        g()\n    except ValueError:\n        return None\n");
        assert!(!f.iter().any(|x| x.kind == "except"));
    }

    #[test]
    fn except_continue_is_not_a_swallow() {
        let f = scan_src("def f(rows):\n    for r in rows:\n        try:\n            parse(r)\n        except ValueError:\n            continue\n    return 1\n");
        assert!(!f.iter().any(|x| x.kind == "except"));
    }

    #[test]
    fn empty_exception_catch_still_fails() {
        let f = scan_src("def f():\n    try:\n        g()\n    except Exception:\n        pass\n");
        assert!(f.iter().any(|x| x.kind == "except"));
    }

    #[test]
    fn accumulator_not_returned_still_swallows() {
        let f = scan_src("def f(rows):\n    issues = []\n    try:\n        parse(rows)\n    except ValueError as e:\n        issues.append(str(e))\n    return 'done'\n");
        assert!(f.iter().any(|x| x.kind == "except")); // issues not returned → swallow
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
    // ------------------------------------------------- record-shape
    #[test]
    fn record_grab_bag_and_collection_params_fail() {
        let f = scan_src("def f(m: dict[str, Any]):\n    return m\n\ndef g(rows: list[dict[str, str]]):\n    return rows\n");
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
        let f = scan_src("def a(x: tuple[str, ...]) -> None:\n    pass\n\ndef b(pair: tuple[str, int]) -> None:\n    pass\n");
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
        assert!(r[0].message.contains("dict literal"));
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
    fn fakefs_real_fs_without_pyfakefs_is_found() {
        let f = scan_src_test("def test_x(tmp_path):\n    p = tmp_path / 'a'\n    p.write_text('hi')\n");
        assert!(f.iter().any(|x| x.kind == "fakefs"));
    }

    #[test]
    fn fakefs_sanctioned_subprocess_need_passes() {
        let f = scan_src_test("import subprocess\n\ndef test_x(tmp_path):\n    subprocess.run(['ls'])\n");
        assert!(!f.iter().any(|x| x.kind == "fakefs"));
    }

    #[test]
    fn record_suppression_with_why_exempts() {
        let f = scan_src("def f(x):\n    return {\"a\": 1, \"b\": x}  # code-health: ignore record-shape genuine map\n");
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
        let root = "repo";
        for (name, src) in files {
            let mut scan = scan_source(src, name);
            scan.file_name = name.to_string();
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
        all.extend(unused_findings(&definitions, &prod_refs, &test_refs, &strings));
        let _ = root;
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
        let (f, c) = (scan_corpus(&[
            ("prod.py", "def seam():\n    return 1\n"),
            ("tests/test_prod.py", "def test_seam():\n    assert seam()\n"),
        ]), ());
        let u: Vec<&Finding> = f.iter().filter(|x| x.kind == "unused").collect();
        assert_eq!(u.len(), 1);
        assert!(u[0].message.contains("referenced only from tests"));
    }
}

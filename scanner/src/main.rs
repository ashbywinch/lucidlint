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
use ruff_python_ast::{Expr, ModModule, Stmt};
use ruff_python_parser::{parse_module, Parsed};
use ruff_text_size::Ranged;
use serde::Serialize;
use std::path::Path;

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
    /// Decision slots: one per open function scope (innermost last).
    fn_stack: Vec<u32>,
    /// Class nesting: class bodies contribute no decisions (radon sub-visitor).
    in_class: u32,
    /// Parent chain for the magic-number position check — exprs plus the
    /// non-expr layers (stmt, keyword) that break the direct-parent link.
    parent_stack: Vec<ParentEntry>,
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
        match stmt {
            Stmt::FunctionDef(f) => {
                let module_level = self.fn_stack.is_empty() && self.in_class == 0;
                let was_fn = self.current_fn.take();
                self.current_fn =
                    Some((f.name.to_string(), line_of(self.source, f.range.start())));
                self.fn_stack.push(0);
                self.unreachable_check(&f.body);
                for s in &f.body {
                    self.visit_stmt(s);
                }
                let cc = self.fn_stack.pop().unwrap_or(0) + 1;
                if module_level {
                    self.cc.push(FnCc {
                        file: self.file.to_string(),
                        function: f.name.to_string(),
                        line: line_of(self.source, f.range.start()),
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
            Stmt::Import(_) | Stmt::ImportFrom(_) => self.import_checks(stmt),
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
                    *self.fn_stack.last_mut().unwrap() += 1 + elifs;
                }
                Stmt::For(f) => {
                    *self.fn_stack.last_mut().unwrap() += 1 + (!f.orelse.is_empty()) as u32
                }
                Stmt::While(w) => {
                    *self.fn_stack.last_mut().unwrap() += 1 + (!w.orelse.is_empty()) as u32
                }
                Stmt::Try(t) => {
                    *self.fn_stack.last_mut().unwrap() +=
                        t.handlers.len() as u32 + (!t.orelse.is_empty()) as u32
                }
                Stmt::Assert(_) => *self.fn_stack.last_mut().unwrap() += 1,
                Stmt::Match(m) => {
                    for case in &m.cases {
                        let wildcard = matches!(
                            &case.pattern,
                            ruff_python_ast::Pattern::MatchAs(p)
                                if p.pattern.is_none() && p.name.is_none()
                        );
                        if !wildcard {
                            *self.fn_stack.last_mut().unwrap() += 1;
                        }
                    }
                }
                _ => {}
            }
        }
        walk_stmt(self, stmt);
        self.parent_stack.pop();
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
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
        if let Some(slot) = self.fn_stack.last_mut() {
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
                Expr::NumberLiteral(n) => self.magic_check(n),
                _ => {}
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

fn scan_file(path: &Path) -> (Vec<Finding>, Vec<FnCc>, usize) {
    let source = std::fs::read_to_string(path).unwrap_or_default();
    let parsed: Parsed<ModModule> = match parse_module(&source) {
        Ok(p) => p,
        Err(_) => return (Vec::new(), Vec::new(), 1),
    };
    let errors = parsed.errors().len();
    let mut state = ScanState {
        file: path.to_str().unwrap_or("<file>"),
        source: &source,
        ..Default::default()
    };
    for stmt in &parsed.syntax().body {
        state.visit_stmt(stmt);
    }
    (state.findings, state.cc, errors)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut all_findings = Vec::new();
    let mut all_cc = Vec::new();
    let mut total_errors = 0usize;
    for path in &args {
        let (findings, cc, errors) = scan_file(Path::new(path));
        all_findings.extend(findings);
        all_cc.extend(cc);
        total_errors += errors;
    }
    let out = serde_json::json!({
        "files": args.len(),
        "parse_errors": total_errors,
        "findings": all_findings,
        "cc": all_cc,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

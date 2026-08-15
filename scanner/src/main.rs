//! code-health-scan — the Rust scan core for the deterministic code-health gate.
//!
//! Phase 1 of the port: parsing + the pure-walk checks (cyclomatic complexity,
//! magic numbers, no-op statements), emitting the same finding shape as the
//! Python tool so the parity gate can diff the two implementations.
//!
//! The finding schema is deliberately language-neutral (file/line/function/
//! kind/severity/message) — JS and other languages plug in later with their
//! own parsers behind the same schema.

use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
use ruff_python_ast::{Expr, ModModule, Stmt};
use ruff_text_size::Ranged;
use ruff_python_parser::{parse_module, Parsed};
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
    /// (name, start line) of the function currently being walked.
    current_fn: Option<(String, usize)>,
    /// Decision points counted for the current function.
    decisions: u32,
    /// 0 = module level; >0 = inside a function being counted. Nested
    /// functions and all class bodies are skipped entirely (radon's
    /// sub-visitor semantics: they contribute nothing to the parent).
    fn_depth: u32,
    /// Parent expressions, for the magic-number position check.
    expr_stack: Vec<Expr>,
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

fn line_of(source: &str, offset: ruff_text_size::TextSize) -> usize {
    1 + source[..offset.to_usize()].bytes().filter(|&b| b == b'\n').count()
}

impl<'a> SourceOrderVisitor<'a> for ScanState<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(f) => {
                if self.fn_depth > 0 {
                    return; // nested function: radon gives it its own visitor — contributes nothing here
                }
                let was = self.current_fn.take();
                self.current_fn = Some((f.name.to_string(), line_of(self.source, f.range.start())));
                self.decisions = 0;
                self.fn_depth = 1;
                for s in &f.body {
                    self.visit_stmt(s);
                }
                self.fn_depth = 0;
                self.cc.push(FnCc {
                    file: self.file.to_string(),
                    function: f.name.to_string(),
                    line: line_of(self.source, f.range.start()),
                    cc: self.decisions + 1,
                });
                self.current_fn = was;
                return;
            }
            Stmt::ClassDef(_) => return, // class bodies (methods) are a sub-visitor in radon: skipped
            Stmt::Expr(e) => self.noop_check(&e.value),
            _ => {}
        }
        // cyclomatic decision points (radon-equivalent CCN)
        match stmt {
            Stmt::If(i) => {
                // radon: if + each elif counts, the trailing else does NOT
                // (visit_If = len(node.ifs) + 1); for/while/try DO count else
                let elifs = i
                    .elif_else_clauses
                    .iter()
                    .filter(|c| c.test.is_some())
                    .count() as u32;
                self.decisions += 1 + elifs
            }
            Stmt::For(f) => self.decisions += 1 + (!f.orelse.is_empty()) as u32,
            Stmt::While(w) => self.decisions += 1 + (!w.orelse.is_empty()) as u32,
            Stmt::Try(t) => {
                self.decisions += t.handlers.len() as u32 + (!t.orelse.is_empty()) as u32
            }
            Stmt::Assert(_) => self.decisions += 1,
            Stmt::Match(m) => {
                for case in &m.cases {
                    let wildcard = matches!(
                        &case.pattern,
                        ruff_python_ast::Pattern::MatchAs(p)
                            if p.pattern.is_none() && p.name.is_none()
                    );
                    if !wildcard {
                        self.decisions += 1;
                    }
                }
            }
            _ => {}
        }
        walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::BoolOp(b) => self.decisions += b.values.len().saturating_sub(1) as u32,
            Expr::If(_) => self.decisions += 1,
            // radon's lambda: +0, but the body IS walked — a ternary or boolop
            // inside the lambda counts (verified empirically on 6.0.1)
            Expr::ListComp(c) => {
                self.decisions += 1 + c.generators.iter().map(|g| g.ifs.len() as u32).sum::<u32>()
            }
            Expr::SetComp(c) => {
                self.decisions += 1 + c.generators.iter().map(|g| g.ifs.len() as u32).sum::<u32>()
            }
            Expr::DictComp(c) => {
                self.decisions += 1 + c.generators.iter().map(|g| g.ifs.len() as u32).sum::<u32>()
            }
            Expr::Generator(c) => {
                self.decisions += 1 + c.generators.iter().map(|g| g.ifs.len() as u32).sum::<u32>()
            }
            Expr::NumberLiteral(n) => self.magic_check(n),
            _ => {}
        }
        self.expr_stack.push(expr.clone());
        walk_expr(self, expr);
        self.expr_stack.pop();
    }
}

impl<'a> ScanState<'a> {
    /// Magic numbers: int/float literals outside (0, 1, 2) whose parent is an
    /// operation — mirrors the Python implementation's position rule. (-1 is
    /// UnaryOp(USub, 1), so the literal itself is always in the skip set.)
    fn magic_check(&mut self, n: &ruff_python_ast::ExprNumberLiteral) {
        let (value, is_num) = match &n.value {
            ruff_python_ast::Number::Int(i) => (i.to_string(), true),
            ruff_python_ast::Number::Float(f) => (f.to_string(), true),
            ruff_python_ast::Number::Complex { .. } => return,
        };
        if !is_num {
            return;
        }
        let skip = matches!(value.as_str(), "0" | "1" | "2");
        if skip {
            return;
        }
        let parent_is_op = matches!(
            self.expr_stack.last(),
            Some(Expr::BinOp(_))
                | Some(Expr::Compare(_))
                | Some(Expr::UnaryOp(_))
                | Some(Expr::Subscript(_))
                | Some(Expr::Call(_))
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
                line: line_of(self.source, expr_range(v).start()),
                function: fn_name,
                kind: "noop-statement".into(),
                severity: "fail".into(),
                message: "expression statement discards its value — dead statement".into(),
            });
        }
    }
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

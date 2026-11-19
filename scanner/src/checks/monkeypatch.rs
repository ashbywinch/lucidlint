//! `monkeypatch`: a test mutates a module or object it does not own — the
//! mutation leaks into every later test in the process.

use ruff_python_ast::{AnyNodeRef, Decorator, Expr, ExprCall, Stmt};
use ruff_text_size::Ranged;
use std::collections::HashSet;

use crate::finding::Finding;
use crate::skeleton::{skel_children, Q};
use crate::source::line_of;
use crate::ScanState;

/// `monkeypatch`: the rule object. Its methods find one patch site; the evidence and the message live together.
pub struct Monkeypatch;

const MONKEYPATCH_METHODS: [&str; 5] = ["setattr", "setitem", "delattr", "setenv", "delenv"];

impl Monkeypatch {
    fn monkeypatch_decorator(state: &mut ScanState, source: &str, d: &Decorator, mock_imports: &HashSet<String>) {
        let expr = &d.expression;
        let func = if let Expr::Call(c) = expr {
            c.func.as_ref()
        } else {
            expr
        };
        let desc: Option<String> = match func {
            Expr::Name(name) if mock_imports.contains(name.id.as_str()) => Some(format!("@{}", name.id.as_str())),
            Expr::Attribute(a) if a.attr.as_str() == "patch" => Some("@patch".to_string()),
            _ => None,
        };
        if let Some(desc) = desc {
            Self::mp_finding(state, &desc, line_of(source, d.range().start()));
        }
    }

    /// Check whether an expression chain (Attribute or Name) resolves to an
    /// imported mock symbol — guards against false positives on e.g. self.client.patch.
    fn attr_resolves_to_mock(val: &Expr, imps: &HashSet<String>) -> bool {
        match val {
            Expr::Name(n) => imps.contains(n.id.as_str()),
            Expr::Attribute(inner) => {
                let seg = inner.attr.to_string();
                imps.contains(&seg) || Self::attr_resolves_to_mock(&inner.value, imps)
            }
            _ => false,
        }
    }

    /// `_monkeypatch_findings` — forbidden patch usage in tests.
    pub fn findings(state: &mut ScanState, body: &[Stmt], source: &str) {
        let mock_imports = Self::mock_import_names(body);
        // calls + decorators in one full walk
        let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
        let mut qi = 0usize;
        while qi < queue.len() {
            if let Q::N(n) = queue[qi] {
                if let AnyNodeRef::ExprCall(call) = n {
                    let line = line_of(source, call.range().start());
                    if let Some(desc) = Self::patch_call(source, call, &mock_imports) {
                        Self::mp_finding(state, &desc, line);
                    }
                }
                // decorators: skel_children descends into the expression, never
                // the Decorator node — examine them at the function/class pop
                if let AnyNodeRef::StmtFunctionDef(f) = n {
                    for d in &f.decorator_list {
                        Self::monkeypatch_decorator(state, source, d, &mock_imports);
                    }
                }
                if let AnyNodeRef::StmtClassDef(cls) = n {
                    for d in &cls.decorator_list {
                        Self::monkeypatch_decorator(state, source, d, &mock_imports);
                    }
                }
                skel_children(n, &mut queue);
            }
            qi += 1;
        }
    }

    fn mp_finding(state: &mut ScanState, desc: &str, line: usize) {
        state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                file: state.file.to_string(),
                line,
                function: String::new(),
                kind: "monkeypatch".into(),
                severity: "fail".into(),
                message: format!(
                    "{desc} at line {line} — never monkeypatch global state; inject an object fake (a class implementing the real protocol) via parameter injection or a dependency-injection container — fakes are objects, not functions"
                ), });
    }

    /// The names this module imported from a mocking library — the vocabulary a
    /// call must use before `.patch` means the library's patcher.
    fn mock_import_names(body: &[Stmt]) -> HashSet<String> {
        let mut names: HashSet<String> = HashSet::new();
        for s in body {
            match s {
                Stmt::ImportFrom(imp) => {
                    if let Some(module) = &imp.module {
                        if module.to_string().contains("mock") {
                            for a in &imp.names {
                                names.insert(a.name.to_string());
                            }
                        }
                    }
                }
                Stmt::Import(imp) => {
                    for a in &imp.names {
                        if a.name.to_string().contains("mock") {
                            names.insert(a.name.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        names
    }

    /// The patcher this call is, when it is one: `monkeypatch.setattr`, or `.patch`
    /// on a receiver chain that resolves to an imported mock symbol — which avoids
    /// false positives on e.g. self.client.patch(url).
    fn patch_call(source: &str, call: &ExprCall, mock_imports: &HashSet<String>) -> Option<String> {
        match call.func.as_ref() {
            Expr::Attribute(a) => match a.value.as_ref() {
                Expr::Name(name) if name.id == "monkeypatch" && MONKEYPATCH_METHODS.contains(&a.attr.as_str()) => {
                    Some(format!("monkeypatch.{}", a.attr.as_str()))
                }
                _ if a.attr.as_str() == "patch" && Self::attr_resolves_to_mock(a.value.as_ref(), mock_imports) => {
                    Some(format!("{}.patch", &source[a.value.range()]))
                }
                _ => None,
            },
            Expr::Name(name) if name.id == "patch" && mock_imports.contains("patch") => Some("patch".to_string()),
            _ => None,
        }
    }
}

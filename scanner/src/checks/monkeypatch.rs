//! `monkeypatch`: a test mutates a module or object it does not own — the
//! mutation leaks into every later test in the process.

use ruff_python_ast::{AnyNodeRef, Decorator, Expr, ExprCall, Stmt};
use ruff_text_size::Ranged;
use std::collections::HashSet;

use crate::skeleton::{skel_children, Q};
use crate::source::Source;
use crate::ScanState;

/// `monkeypatch`: the rule object. Its methods find one patch site; the evidence and the message live together.
pub struct Monkeypatch;

/// One patch site: the dotted target when the call names one as a string, the
/// call shape, and where it is. The repo-wide pass decides whether the target
/// is a module the repo owns or a third-party dependency.
#[derive(Clone)]
pub struct PatchSite {
    pub target: Option<String>,
    pub desc: String,
    pub line: usize,
    pub function: String,
}

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
            let target = if let Expr::Call(call) = expr {
                Self::call_target(call)
            } else {
                None
            };
            Self::mp_site(state, desc, target, Source::line_of(source, d.range().start()));
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
                    let line = Source::line_of(source, call.range().start());
                    if let Some((desc, target)) = Self::patch_call(source, call, &mock_imports) {
                        Self::mp_site(state, desc, target, line);
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

    fn mp_site(state: &mut ScanState, desc: String, target: Option<String>, line: usize) {
        let function = state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        state.patch_sites.push(PatchSite {
            target,
            desc,
            line,
            function,
        });
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
    fn patch_call(source: &str, call: &ExprCall, mock_imports: &HashSet<String>) -> Option<(String, Option<String>)> {
        match call.func.as_ref() {
            Expr::Attribute(a) => match a.value.as_ref() {
                Expr::Name(name) if name.id == "monkeypatch" && MONKEYPATCH_METHODS.contains(&a.attr.as_str()) => {
                    Some((format!("monkeypatch.{}", a.attr.as_str()), Self::call_target(call)))
                }
                _ if a.attr.as_str() == "patch" && Self::attr_resolves_to_mock(a.value.as_ref(), mock_imports) => {
                    Some((format!("{}.patch", &source[a.value.range()]), Self::call_target(call)))
                }
                _ => None,
            },
            Expr::Name(name) if name.id == "patch" && mock_imports.contains("patch") => {
                Some(("patch".to_string(), Self::call_target(call)))
            }
            _ => None,
        }
    }

    /// The first argument when the call names its target as a string — the
    /// dotted module path the repo-wide pass resolves. An object target (a
    /// Name/Attribute) is a local object, not a module path.
    fn call_target(call: &ExprCall) -> Option<String> {
        match call.arguments.args.first()? {
            Expr::StringLiteral(s) => Some(s.value.to_str().to_string()),
            _ => None,
        }
    }
}

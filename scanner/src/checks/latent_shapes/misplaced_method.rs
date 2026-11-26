//! `misplaced-method`: a module-level function called from a method with
//! `self.<attr>` arguments (or locals named like the class's attributes)
//! matching EVERY parameter — the class already holds the data, so the function
//! is its method in exile.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::visitor::source_order::{walk_expr, SourceOrderVisitor};
use ruff_python_ast::{Expr, Stmt, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

use super::super::{class_attr_names, push_stmt_children, stmt_exprs};

/// What one returned expression does with the parameter: reads through it,
/// uses it as a bare value, or contains a conditional.
struct Derivation<'p> {
    param: &'p str,
    reads: usize,
    bare: bool,
    decision: bool,
}

impl<'p> Derivation<'p> {
    fn of(expr: &Expr, param: &'p str) -> Self {
        struct Probe<'p> {
            usage: Derivation<'p>,
        }
        impl<'a, 'p> SourceOrderVisitor<'a> for Probe<'p> {
            fn visit_expr(&mut self, expr: &'a Expr) {
                match expr {
                    // ruff's conditional expression is `Expr::If`; there is no
                    // separate `IfExpression` variant.
                    Expr::If(_) => self.usage.decision = true,
                    Expr::Attribute(a) if matches!(a.value.as_ref(), Expr::Name(n) if n.id.as_str() == self.usage.param) =>
                    {
                        self.usage.reads += 1;
                        return;
                    }
                    Expr::Name(n) if n.id.as_str() == self.usage.param => self.usage.bare = true,
                    _ => {}
                }
                walk_expr(self, expr);
            }
        }
        let mut probe = Probe {
            usage: Self {
                param,
                reads: 0,
                bare: false,
                decision: false,
            },
        };
        probe.visit_expr(expr);
        probe.usage
    }
}

/// The misplaced-method rule. Its `findings` walk is the rule's only entry.
pub struct MisplacedMethod;

impl MisplacedMethod {
    /// The class an annotation names, when it is one this module defines.
    fn annotation_class(annotation: Option<&Expr>, classes: &HashSet<String>) -> Option<String> {
        let name = match annotation? {
            Expr::Name(n) => n.id.to_string(),
            Expr::StringLiteral(s) => s.value.to_str().to_string(),
            _ => return None,
        };
        classes.contains(&name).then_some(name)
    }

    /// What one returned expression does with the parameter: reads through it,
    /// uses it as a bare value, or contains a conditional.
    fn derivation<'p>(expr: &Expr, param: &'p str) -> Derivation<'p> {
        Derivation::of(expr, param)
    }

    /// Every module-level function: its name, line, and parameters.
    fn module_functions(body: &[Stmt], source: &str) -> HashMap<String, (usize, Vec<String>)> {
        body.iter()
            .filter_map(|s| match s {
                Stmt::FunctionDef(f) => Some((
                    f.name.to_string(),
                    (Source::line_of(source, f.name.range().start()), Self::param_names(f)),
                )),
                _ => None,
            })
            .collect()
    }

    fn param_names(f: &StmtFunctionDef) -> Vec<String> {
        let mut params: Vec<String> = f
            .parameters
            .posonlyargs
            .iter()
            .chain(&f.parameters.args)
            .chain(&f.parameters.kwonlyargs)
            .map(|a| a.parameter.name.to_string())
            .collect();
        if let Some(v) = &f.parameters.vararg {
            params.push(v.name.to_string());
        }
        if let Some(k) = &f.parameters.kwarg {
            params.push(k.name.to_string());
        }
        params
    }

    /// Report every module function that receives the class's own state.
    pub fn findings(state: &mut ScanState, body: &[Stmt]) {
        let module_fns = Self::module_functions(body, state.source);
        if module_fns.is_empty() {
            return;
        }
        let module_classes: HashSet<String> = body
            .iter()
            .filter_map(|s| match s {
                Stmt::ClassDef(c) => Some(c.name.to_string()),
                _ => None,
            })
            .collect();
        let mut reported: HashSet<usize> = HashSet::new();
        for s in body {
            let Stmt::ClassDef(c) = s else { continue };
            let attrs = class_attr_names(&c.body);
            if attrs.is_empty() {
                continue;
            }
            for m in &c.body {
                let Stmt::FunctionDef(method) = m else { continue };
                let mut stack: Vec<&Stmt> = Vec::new();
                stack.extend(method.body.iter());
                while let Some(st) = stack.pop() {
                    if matches!(st, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
                        continue;
                    }
                    for e in stmt_exprs(st) {
                        let Expr::Call(call) = e else { continue };
                        let Expr::Name(fn_name) = call.func.as_ref() else {
                            continue;
                        };
                        let Some((def_line, params)) = module_fns.get(fn_name.id.as_str()) else {
                            continue;
                        };
                        if params.is_empty() || call.arguments.args.len() < params.len() {
                            continue;
                        }
                        let matched = params.iter().enumerate().all(|(i, p)| {
                            call.arguments.args.get(i).is_some_and(|arg| match arg {
                                Expr::Attribute(a) => {
                                    matches!(a.value.as_ref(), Expr::Name(n) if n.id.as_str() == "self")
                                        && a.attr.as_str() == p.as_str()
                                }
                                Expr::Name(n) => n.id.as_str() == p.as_str() && attrs.contains(n.id.as_str()),
                                _ => false,
                            })
                        });
                        if matched && reported.insert(*def_line) {
                            state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                                        file: state.file.to_string(),
                                                        line: *def_line,
                                                        function: fn_name.id.to_string(),
                                                        kind: "misplaced-method".into(),
                                                        severity: "fail".into(),
                                                        message: format!(
                                                            "'{}' is called from '{}.{}' with the class's own state (self.{}) — the class already holds the data; move the function onto the class",
                                                            fn_name.id.as_str(),
                                                            c.name.as_str(),
                                                            method.name.as_str(),
                                                            params.join(", self.")
                                                        ), });
                        }
                    }
                    push_stmt_children(st, &mut stack);
                }
            }
        }

        Self::direct_form(state, body, &module_classes, &mut reported);
    }

    /// The one returned expression (after an optional docstring), when the body
    /// is exactly that.
    fn single_return(body: &[Stmt]) -> Option<&Expr> {
        let mut statements = body.iter().filter(|s| !Self::is_docstring(s));
        let Stmt::Return(ret) = statements.next()? else {
            return None;
        };
        if statements.next().is_some() {
            return None;
        }
        ret.value.as_deref()
    }

    fn is_docstring(stmt: &Stmt) -> bool {
        matches!(stmt, Stmt::Expr(e) if matches!(e.value.as_ref(), Expr::StringLiteral(_)))
    }

    /// The only parameter, when the function takes exactly one positional
    /// parameter and no keyword or varargs: a service with more inputs is not a
    /// derivation of the object.
    fn only_parameter(f: &StmtFunctionDef) -> Option<&ruff_python_ast::ParameterWithDefault> {
        let positional = f.parameters.posonlyargs.len() + f.parameters.args.len();
        if positional != 1
            || !f.parameters.kwonlyargs.is_empty()
            || f.parameters.vararg.is_some()
            || f.parameters.kwarg.is_some()
        {
            return None;
        }
        f.parameters.posonlyargs.first().or_else(|| f.parameters.args.first())
    }

    /// A module function that takes one of the module's classes as its first
    /// parameter and only reads through it is that class's method, with no call
    /// site needed.
    fn direct_form(
        state: &mut ScanState,
        body: &[Stmt],
        module_classes: &HashSet<String>,
        reported: &mut HashSet<usize>,
    ) {
        for s in body {
            let Stmt::FunctionDef(f) = s else { continue };
            if !f.decorator_list.is_empty() {
                continue;
            }
            let Some(param) = Self::only_parameter(f) else {
                continue;
            };
            let Some(annotation) = param.parameter.annotation.as_deref() else {
                continue;
            };
            let Some(class) = Self::annotation_class(Some(annotation), module_classes) else {
                continue;
            };
            let Some(value) = Self::single_return(&f.body) else {
                continue;
            };
            let usage = Self::derivation(value, param.parameter.name.as_str());
            if usage.reads < 2 || usage.bare || usage.decision {
                continue;
            }
            let line = Source::line_of(state.source, f.name.range().start());
            if !reported.insert(line) {
                continue;
            }
            state.findings.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                col: 0,
                file: state.file.to_string(),
                line,
                function: f.name.to_string(),
                kind: "derived-method".into(),
                severity: "warn".into(),
                message: format!(
                    "'{}' takes only a '{}' and reads its fields. If it is likely to change alongside '{}', and unlikely to change alongside '{}', it should be a member of '{}'.",
                    f.name.as_str(),
                    class,
                    class,
                    state.file,
                    class
                ),
            });
        }
    }
}

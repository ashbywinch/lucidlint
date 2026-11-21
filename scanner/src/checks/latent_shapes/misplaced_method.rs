//! `misplaced-method`: a module-level function called from a method with
//! `self.<attr>` arguments (or locals named like the class's attributes)
//! matching EVERY parameter — the class already holds the data, so the function
//! is its method in exile.

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

use super::super::{class_attr_names, push_stmt_children, stmt_exprs};

/// The misplaced-method rule. Its `findings` walk is the rule's only entry.
pub struct MisplacedMethod;

impl MisplacedMethod {
    /// Report every module function that receives the class's own state.
    pub fn findings(state: &mut ScanState, body: &[Stmt]) {
        let mut module_fns: std::collections::HashMap<String, (usize, Vec<String>)> = std::collections::HashMap::new();
        for s in body {
            let Stmt::FunctionDef(f) = s else { continue };
            let mut params = Vec::new();
            params.extend(
                f.parameters
                    .posonlyargs
                    .iter()
                    .chain(&f.parameters.args)
                    .chain(&f.parameters.kwonlyargs)
                    .map(|a| a.parameter.name.to_string()),
            );
            if let Some(v) = &f.parameters.vararg {
                params.push(v.name.to_string());
            }
            if let Some(k) = &f.parameters.kwarg {
                params.push(k.name.to_string());
            }
            let line = Source::line_of(state.source, f.name.range().start());
            module_fns.insert(f.name.to_string(), (line, params));
        }
        if module_fns.is_empty() {
            return;
        }
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
                        if matched {
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
    }
}

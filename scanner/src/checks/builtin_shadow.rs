//! `builtin-shadow`: a parameter or local that shadows the language's own
//! vocabulary — the reader now has to remember which `list` is meant.
// lucidlint: ignore-file complexity the shadow walker is a dispatch table over the
// statement kinds — arm count is the AST's surface, not branching

use ruff_python_ast::{Expr, Stmt};

use crate::finding::Finding;
use crate::{stmt_line, ScanState};

pub const SHADOWED_BUILTINS: &[&str] = &[
    "abs",
    "all",
    "any",
    "bin",
    "bool",
    "bytes",
    "callable",
    "chr",
    "classmethod",
    "complex",
    "dict",
    "dir",
    "divmod",
    "enumerate",
    "eval",
    "exec",
    "filter",
    "float",
    "format",
    "frozenset",
    "getattr",
    "globals",
    "hasattr",
    "hash",
    "hex",
    "id",
    "input",
    "int",
    "isinstance",
    "issubclass",
    "iter",
    "len",
    "list",
    "locals",
    "map",
    "max",
    "memoryview",
    "min",
    "next",
    "object",
    "oct",
    "open",
    "ord",
    "pow",
    "print",
    "property",
    "range",
    "repr",
    "reversed",
    "round",
    "set",
    "setattr",
    "slice",
    "sorted",
    "staticmethod",
    "str",
    "sum",
    "super",
    "tuple",
    "type",
    "vars",
    "zip",
];

/// Parameters and local assignments shadowing builtins.
pub fn shadow_findings(state: &mut ScanState, stmt: &Stmt) {
    if let Stmt::FunctionDef(f) = stmt {
        for a in &f.parameters.posonlyargs {
            if SHADOWED_BUILTINS.contains(&a.parameter.name.as_str()) {
                state.findings.push(Finding {
                    logical_start: None,
                    seam_members: Vec::new(),
                    col: 0,
                    file: state.file.to_string(),
                    line: stmt_line(state.source, stmt),
                    function: f.name.to_string(),
                    kind: "builtin-shadow".into(),
                    severity: "fail".into(),
                    message: format!(
                        "parameter '{}' shadows a builtin — rename it",
                        a.parameter.name.as_str()
                    ),
                });
            }
        }
        for a in &f.parameters.args {
            if SHADOWED_BUILTINS.contains(&a.parameter.name.as_str()) {
                state.findings.push(Finding {
                    logical_start: None,
                    seam_members: Vec::new(),
                    col: 0,
                    file: state.file.to_string(),
                    line: stmt_line(state.source, stmt),
                    function: f.name.to_string(),
                    kind: "builtin-shadow".into(),
                    severity: "fail".into(),
                    message: format!(
                        "parameter '{}' shadows a builtin — rename it",
                        a.parameter.name.as_str()
                    ),
                });
            }
        }
        for a in &f.parameters.kwonlyargs {
            if SHADOWED_BUILTINS.contains(&a.parameter.name.as_str()) {
                state.findings.push(Finding {
                    logical_start: None,
                    seam_members: Vec::new(),
                    col: 0,
                    file: state.file.to_string(),
                    line: stmt_line(state.source, stmt),
                    function: f.name.to_string(),
                    kind: "builtin-shadow".into(),
                    severity: "fail".into(),
                    message: format!(
                        "parameter '{}' shadows a builtin — rename it",
                        a.parameter.name.as_str()
                    ),
                });
            }
        }
        if let Some(v) = &f.parameters.vararg {
            if SHADOWED_BUILTINS.contains(&v.name.as_str()) {
                state.findings.push(Finding {
                    logical_start: None,
                    seam_members: Vec::new(),
                    col: 0,
                    file: state.file.to_string(),
                    line: stmt_line(state.source, stmt),
                    function: f.name.to_string(),
                    kind: "builtin-shadow".into(),
                    severity: "fail".into(),
                    message: format!("parameter '{}' shadows a builtin — rename it", v.name.as_str()),
                });
            }
        }
        if let Some(k) = &f.parameters.kwarg {
            if SHADOWED_BUILTINS.contains(&k.name.as_str()) {
                state.findings.push(Finding {
                    logical_start: None,
                    seam_members: Vec::new(),
                    col: 0,
                    file: state.file.to_string(),
                    line: stmt_line(state.source, stmt),
                    function: f.name.to_string(),
                    kind: "builtin-shadow".into(),
                    severity: "fail".into(),
                    message: format!("parameter '{}' shadows a builtin — rename it", k.name.as_str()),
                });
            }
        }
    }
    if let Stmt::Assign(a) = stmt {
        if state.fn_stack.is_empty() {
            return;
        }
        for target in &a.targets {
            if let Expr::Name(n) = target {
                if SHADOWED_BUILTINS.contains(&n.id.as_str()) {
                    let fn_name = state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
                    state.findings.push(Finding {
                        logical_start: None,
                        seam_members: Vec::new(),
                        col: 0,
                        file: state.file.to_string(),
                        line: stmt_line(state.source, stmt),
                        function: fn_name,
                        kind: "builtin-shadow".into(),
                        severity: "fail".into(),
                        message: format!("variable '{}' shadows a builtin — rename it", n.id.as_str()),
                    });
                }
            }
        }
    }
}

// =====================================================================
// signature hygiene: positional boolean literals (boolean-arg) and
// over-long parameter lists (long-param-list)
// =====================================================================

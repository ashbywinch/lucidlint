//! `builtin-shadow`: a parameter or local that shadows the language's own
//! vocabulary — the reader now has to remember which `list` is meant.
// lucidlint: ignore-file complexity the shadow walker is a dispatch table over the
// statement kinds — arm count is the AST's surface, not branching

use ruff_python_ast::{ExceptHandler, Expr, Stmt};

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

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

/// One shadow finding, when `name` is a builtin. `role` is the word the message
/// uses for the binding: `parameter` or `variable`.
fn push_shadow(state: &mut ScanState, name: &str, line: usize, function: &str, role: &str) {
    if !SHADOWED_BUILTINS.contains(&name) {
        return;
    }
    state.findings.push(Finding {
        logical_start: None,
        seam_members: Vec::new(),
        col: 0,
        file: state.file.to_string(),
        line,
        function: function.to_string(),
        kind: "builtin-shadow".into(),
        severity: "fail".into(),
        message: format!("{role} '{name}' shadows a builtin — rename it"),
    });
}

/// The names a binding target introduces: `a`, `(a, b)`, `[a, *b]`.
fn target_names<'a>(target: &'a Expr, out: &mut Vec<&'a str>) {
    match target {
        Expr::Name(n) => out.push(n.id.as_str()),
        Expr::Tuple(t) => t.elts.iter().for_each(|e| target_names(e, out)),
        Expr::List(l) => l.elts.iter().for_each(|e| target_names(e, out)),
        Expr::Starred(s) => target_names(&s.value, out),
        _ => {}
    }
}

/// Every binding that shadows a builtin: parameters, local assignments, `for`
/// targets, `with ... as` names and `except ... as` names. A module-scope
/// binding is `global-state`'s finding, so assignments are read only inside a
/// function.
pub fn shadow_findings(state: &mut ScanState, stmt: &Stmt) {
    let line = Source::stmt_line(state.source, stmt);
    let mut names: Vec<&str> = Vec::new();
    let mut role = "variable";
    let mut function = state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
    match stmt {
        Stmt::FunctionDef(f) => {
            role = "parameter";
            function = f.name.to_string();
            names.extend(f.parameters.posonlyargs.iter().map(|a| a.parameter.name.as_str()));
            names.extend(f.parameters.args.iter().map(|a| a.parameter.name.as_str()));
            names.extend(f.parameters.kwonlyargs.iter().map(|a| a.parameter.name.as_str()));
            names.extend(f.parameters.vararg.iter().map(|v| v.name.as_str()));
            names.extend(f.parameters.kwarg.iter().map(|k| k.name.as_str()));
        }
        Stmt::Assign(a) => {
            if state.fn_stack.is_empty() {
                return; // module scope: global-state's finding
            }
            for target in &a.targets {
                target_names(target, &mut names);
            }
        }
        Stmt::For(f) => target_names(&f.target, &mut names),
        Stmt::With(w) => {
            for item in &w.items {
                if let Some(vars) = &item.optional_vars {
                    target_names(vars, &mut names);
                }
            }
        }
        Stmt::Try(t) => {
            for handler in &t.handlers {
                let ExceptHandler::ExceptHandler(eh) = handler;
                if let Some(name) = &eh.name {
                    names.push(name.as_str());
                }
            }
        }
        _ => return,
    }
    for name in names {
        push_shadow(state, name, line, &function, role);
    }
}

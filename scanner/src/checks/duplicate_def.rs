//! `duplicate-def`: a module-scope name bound twice — the later definition
//! silently shadows the earlier, which is legal Python and invisible to ruff.

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt};

use crate::{finding::Finding, stmt_line, ScanState};

/// Module-scope name reuse: a def/class/import binding that shadows an earlier
/// module-scope binding of the same name (review-log §10, finding 1 — the
/// `guess_pages` CLI command shadowing the module helper; also the
/// double-`def` edit mistake). Legal Python — the later definition wins — so
/// neither ruff nor pyrefly flags it, and the name-based ref graph cannot see
/// it either.
pub fn duplicate_def_findings(state: &mut ScanState, module_body: &[Stmt]) {
    let (overloaded, impl_def_line, overload_bound) = overload_exemption(module_body, state.source);
    let mut seen: Vec<(String, usize)> = Vec::new();
    for s in module_body {
        let line = stmt_line(state.source, s);
        for (name, is_def, stub) in bound_names(s, &overload_bound) {
            if name.is_empty() {
                continue;
            }
            if is_def {
                let exempt = stub || (overloaded.contains(&name) && impl_def_line.get(&name).copied() == Some(line));
                if !exempt {
                    if let Some((_, first_line)) = seen.iter().find(|(n, _)| *n == name) {
                        state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                                    file: state.file.to_string(),
                                                    line,
                                                    function: name.clone(),
                                                    kind: "duplicate-def".into(),
                                                    severity: "fail".into(),
                                                    message: format!(
                                                        "module-scope name '{name}' is bound twice (first at line {first_line}) — the later definition shadows the earlier: a shadowing hazard (dispatch or edit mistake); rename one — fix: duplicate-def"
                                                    ), });
                    }
                }
            }
            seen.push((name, line));
        }
    }
}

/// The names one module-scope statement binds: `(name, is the binder a
/// def/class?, is it an @overload stub?)`.
fn bound_names(s: &Stmt, overload_bound: &HashSet<String>) -> Vec<(String, bool, bool)> {
    let mut bindings: Vec<(String, bool, bool)> = Vec::new();
    match s {
        Stmt::FunctionDef(f) => bindings.push((f.name.to_string(), true, is_overload_stub(f, overload_bound))),
        Stmt::ClassDef(c) => bindings.push((c.name.to_string(), true, false)),
        Stmt::Import(i) => {
            for a in &i.names {
                let bound = a
                    .asname
                    .as_ref()
                    .map(|n| n.id.to_string())
                    .unwrap_or_else(|| a.name.split('.').next().unwrap_or("").to_string());
                bindings.push((bound, false, false));
            }
        }
        Stmt::ImportFrom(fr) => {
            for a in &fr.names {
                if a.name.as_str() == "*" {
                    continue;
                }
                // only a DEF/CLASS shadow is a hazard worth flagging: `import
                // urllib` + `import urllib.request` both bind `urllib`
                // idiomatically, and the §10 def-in-imports mistake is exactly
                // a def landing on an import's name
                let bound = a
                    .asname
                    .as_ref()
                    .map(|n| n.id.to_string())
                    .unwrap_or_else(|| a.name.to_string());
                bindings.push((bound, false, false));
            }
        }
        _ => {}
    }
    bindings
}

/// Is this def one of the `@overload` stubs the exemption covers?
fn is_overload_stub(f: &ruff_python_ast::StmtFunctionDef, overload_bound: &HashSet<String>) -> bool {
    f.decorator_list.iter().any(|d| {
        matches!(&d.expression, Expr::Name(n) if overload_bound.contains(n.id.as_str()))
            || matches!(&d.expression, Expr::Attribute(a) if a.attr.as_str() == "overload")
    })
}

/// The @overload exemption pre-scan: the bound decorator names (aliases
/// resolved — `overload as ov` binds `ov`), the overloaded def names, and
/// each overloaded name's LAST def line (the implementation). Only the
/// stubs and that one impl are exempt; a further def of the name is a
/// genuine duplicate (review finding).
fn overload_exemption(
    module_body: &[Stmt],
    source: &str,
) -> (
    std::collections::HashSet<String>,
    std::collections::HashMap<String, usize>,
    std::collections::HashSet<String>,
) {
    let mut overload_bound: std::collections::HashSet<String> =
        std::collections::HashSet::from(["overload".to_string()]);
    for s in module_body {
        if let Stmt::ImportFrom(fr) = s {
            for a in &fr.names {
                if a.name.as_str() == "overload" || a.name.as_str() == "*" {
                    overload_bound.insert(
                        a.asname
                            .as_ref()
                            .map(|n| n.id.to_string())
                            .unwrap_or_else(|| "overload".to_string()),
                    );
                }
            }
        }
    }
    let mut overloaded: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut impl_def_line: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for s in module_body {
        if let Stmt::FunctionDef(f) = s {
            let name = f.name.to_string();
            let is_overload = f.decorator_list.iter().any(|d| {
                matches!(&d.expression, Expr::Name(n) if overload_bound.contains(n.id.as_str()))
                    || matches!(&d.expression, Expr::Attribute(a) if a.attr.as_str() == "overload")
            });
            if is_overload {
                overloaded.insert(name.clone());
            } else {
                // the FIRST non-overload def is the implementation the stubs
                // declare; tracking the LAST def instead exempted a genuine
                // duplicate after the impl and flagged the impl (review bot)
                impl_def_line.entry(name).or_insert(stmt_line(source, s));
            }
        }
    }
    (overloaded, impl_def_line, overload_bound)
}

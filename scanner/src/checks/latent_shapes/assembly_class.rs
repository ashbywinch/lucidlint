//! `assembly-class`: a function assembles >=3 structures by threading the same
//! data through module-level functions whose outputs feed each other's inputs —
//! a class in waiting: the threaded state belongs on an object.

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

use super::super::push_stmt_children;

/// The assembly-class rule: the module functions and the threading they share.
pub struct AssemblyClass;

impl AssemblyClass {
    /// Report every function that threads shared data through two or more
    /// module functions into three or more results.
    pub fn findings(state: &mut ScanState, body: &[Stmt]) {
        let module_fns: HashSet<String> = body
            .iter()
            .filter_map(|s| match s {
                Stmt::FunctionDef(f) => Some(f.name.to_string()),
                _ => None,
            })
            .collect();
        if module_fns.len() < 2 {
            return;
        }
        let mut fns: Vec<(&StmtFunctionDef, usize)> = Vec::new();
        for s in body {
            match s {
                Stmt::FunctionDef(f) => fns.push((f, Source::line_of(state.source, f.name.range().start()))),
                Stmt::ClassDef(c) => {
                    for m in &c.body {
                        if let Stmt::FunctionDef(f) = m {
                            fns.push((f, Source::line_of(state.source, f.name.range().start())));
                        }
                    }
                }
                _ => {}
            }
        }
        for (f, line) in fns {
            let mut results: HashSet<String> = HashSet::new();
            let mut calls: Vec<(String, Vec<String>)> = Vec::new();
            let mut stack: Vec<&Stmt> = Vec::new();
            for b in &f.body {
                stack.push(b);
            }
            while let Some(s) = stack.pop() {
                if matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
                    continue;
                }
                if let Stmt::Assign(a) = s {
                    if let Expr::Call(c) = a.value.as_ref() {
                        if let Expr::Name(n) = c.func.as_ref() {
                            if module_fns.contains(n.id.as_str()) {
                                let args: Vec<String> = c
                                    .arguments
                                    .args
                                    .iter()
                                    .filter_map(|x| match x {
                                        Expr::Name(m) => Some(m.id.to_string()),
                                        _ => None,
                                    })
                                    .collect();
                                calls.push((n.id.to_string(), args));
                                for t in &a.targets {
                                    match t {
                                        Expr::Name(rn) => {
                                            results.insert(rn.id.to_string());
                                        }
                                        Expr::Tuple(tp) => {
                                            for el in &tp.elts {
                                                if let Expr::Name(rn) = el {
                                                    results.insert(rn.id.to_string());
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
                push_stmt_children(s, &mut stack);
            }
            if calls.len() < 2 || results.len() < 3 {
                continue;
            }
            // chain: one call's argument is another call's result
            let chain = calls.iter().any(|(_, args)| args.iter().any(|a| results.contains(a)));
            if !chain {
                continue;
            }
            // shared base: a non-result argument name appearing in >= 2 calls
            let mut base: Option<String> = None;
            'outer: for (_, args) in &calls {
                for a in args {
                    if results.contains(a) {
                        continue;
                    }
                    let n = calls.iter().filter(|(_, as2)| as2.iter().any(|x| x == a)).count();
                    if n >= 2 {
                        base = Some(a.clone());
                        break 'outer;
                    }
                }
            }
            let Some(base) = base else { continue };
            let chain_names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();
            state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                        file: state.file.to_string(),
                        line,
                        function: f.name.to_string(),
                        kind: "assembly-class".into(),
                        severity: "fail".into(),
                        message: format!(
                            "'{}' assembles {} structures by threading '{}' through {} ({} → {}) — a class in waiting: the threaded state belongs on an object, the module functions are its methods",
                            f.name.as_str(),
                            results.len(),
                            base,
                            calls.len(),
                            chain_names.join(" → "),
                            chain_names.last().copied().unwrap_or("")
                        ), });
        }
    }
}

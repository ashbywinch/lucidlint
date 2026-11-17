//! `forwarding-chain`: a method that forwards to a module function that
//! forwards to another class's method — the chain hides the work.

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::finding::Finding;

use super::husks::{fn_param_names, is_pure_forwarder_body};
use crate::forwarding::{ForwarderFn, ForwarderMethod, RepoMethod};
use crate::{line_of, ScanState};

/// The method-callee forwarder form: `recv.method(<all params except the
/// receiver>)` with the receiver the leading param.
fn method_forwarder_callee(body: &[Stmt], params: &[String]) -> Option<(String, String)> {
    if body.len() != 1 {
        return None;
    }
    let Stmt::Return(r) = &body[0] else { return None };
    let value = r.value.as_deref()?;
    let Expr::Call(c) = value else { return None };
    let Expr::Attribute(a) = c.func.as_ref() else {
        return None;
    };
    if !c.arguments.keywords.is_empty() {
        return None;
    }
    let Expr::Name(recv) = a.value.as_ref() else {
        return None;
    };
    let first = params.first()?;
    if recv.id.as_str() != first.as_str() {
        return None;
    }
    if c.arguments.args.len() != params.len().saturating_sub(1) {
        return None;
    }
    for (arg, param) in c.arguments.args.iter().zip(params.iter().skip(1)) {
        let Expr::Name(an) = arg else { return None };
        if an.id.as_str() != param.as_str() {
            return None;
        }
    }
    Some((first.clone(), a.attr.to_string()))
}

/// Collect a file's forwarding-chain raw material (#30): module-level
/// functions that are pure forwarders (the middle edge), class methods that
/// forward to a module-level function (the head edge), and EVERY class
/// method (the terminal pool the Method: callee resolves against).
pub fn collect_forwarders(state: &mut ScanState, body: &[Stmt]) {
    let module_fn_params: std::collections::HashMap<String, Vec<String>> = body
        .iter()
        .filter_map(|s| match s {
            Stmt::FunctionDef(f) => Some((f.name.to_string(), fn_param_names(f))),
            _ => None,
        })
        .collect();
    for s in body {
        let Stmt::FunctionDef(f) = s else { continue };
        let params = fn_param_names(f);
        let mut name_callee = String::new();
        let is_head = is_pure_forwarder_body(&f.body, &mut name_callee, &params);
        let method_callee = method_forwarder_callee(&f.body, &params);
        if is_head || method_callee.is_some() {
            state.forwarder_fns.push(ForwarderFn {
                rel: state.file.to_string(),
                name: f.name.to_string(),
                line: line_of(state.source, f.name.range().start()),
                params: params.clone(),
                callee: match method_callee {
                    Some((recv, m)) => format!("Method:{recv}:{m}"),
                    None => format!("Fn:{name_callee}"),
                },
            });
        }
    }
    for s in body {
        let Stmt::ClassDef(cls) = s else { continue };
        for m in &cls.body {
            let Stmt::FunctionDef(f) = m else { continue };
            let params: Vec<String> = fn_param_names(f)
                .into_iter()
                .filter(|p| p != "self" && p != "cls")
                .collect();
            let mut callee = String::new();
            if is_pure_forwarder_body(&f.body, &mut callee, &params) && module_fn_params.contains_key(&callee) {
                state.forwarder_methods.push(ForwarderMethod {
                    rel: state.file.to_string(),
                    class: cls.name.to_string(),
                    name: f.name.to_string(),
                    line: line_of(state.source, f.name.range().start()),
                    params: params.clone(),
                    fn_callee: callee,
                });
            }
            state.repo_methods.push(RepoMethod {
                rel: state.file.to_string(),
                class: cls.name.to_string(),
                name: f.name.to_string(),
                line: line_of(state.source, f.name.range().start()),
                params,
            });
        }
    }
}

/// Does `rel`'s module already depend on the module of `target_rel`
/// (import/reference present, or the same module)? The #30 dependency gate:
/// collapsing a chain would marry two independent modules when no dependency
/// exists, so only already-coupled modules fire.
fn depends_on(
    rel: &str,
    target_rel: &str,
    imports_by_rel: &std::collections::HashMap<String, Vec<crate::ImportInfo>>,
) -> bool {
    if rel == target_rel {
        return true;
    }
    let target_stem = target_rel
        .rsplit('/')
        .next()
        .unwrap_or(target_rel)
        .trim_end_matches(".py")
        .to_lowercase();
    imports_by_rel.get(rel).is_some_and(|imps| {
        imps.iter()
            .any(|imp| imp.module.rsplit('.').next().unwrap_or(&imp.module).to_lowercase() == target_stem)
    })
}

/// Forwarding chains (#30): method M (class C, file F1) whose body is
/// exactly `return F(<all M params>)`, module fn F whose body is exactly
/// `return N(<all F params>)`, N a method on ANOTHER class D (or a module
/// fn at depth >= 2 — the #27 handoff shape). Emits ONLY when C's module
/// already depends on D's module — a chain that is the ONLY coupling
/// between two otherwise-independent modules is a deliberate boundary and
/// gets NO finding and NO message.
pub fn forwarding_chain_findings(
    heads: &[ForwarderMethod],
    fns: &[ForwarderFn],
    repo_methods: &[RepoMethod],
    imports_by_rel: &std::collections::HashMap<String, Vec<crate::ImportInfo>>,
) -> Vec<Finding> {
    use std::collections::HashMap;
    let mut fn_callees: HashMap<String, &ForwarderFn> = HashMap::new();
    for f in fns {
        fn_callees.entry(f.name.clone()).or_insert(f);
    }
    let mut methods_by_name: HashMap<String, Vec<&RepoMethod>> = HashMap::new();
    for m in repo_methods {
        methods_by_name.entry(m.name.clone()).or_default().push(m);
    }
    let mut out = Vec::new();
    for head in heads {
        let Some(mid) = fn_callees.get(&head.fn_callee).copied() else {
            continue;
        };
        let message = if let Some(rest) = mid.callee.strip_prefix("Method:") {
            let mut it = rest.splitn(2, ':');
            let recv = it.next().unwrap_or("").to_string();
            let method = it.next().unwrap_or("").to_string();
            // N is a method on another class D — resolve structurally: the
            // unique repo method whose params match the forwarded args
            let arg_params: Vec<String> = mid
                .params
                .iter()
                .filter(|p| p.as_str() != recv.as_str())
                .cloned()
                .collect();
            let candidates: Vec<&RepoMethod> = methods_by_name
                .get(&method)
                .into_iter()
                .flatten()
                .filter(|m| m.params == arg_params && m.class != head.class)
                .copied()
                .collect();
            if candidates.len() != 1 {
                continue; // ambiguous or unresolvable — no structural certainty
            }
            let d = candidates[0];
            if !depends_on(&head.rel, &d.rel, imports_by_rel) {
                continue; // the deliberate boundary — silent
            }
            format!(
                "method '{}.{}({})' ({}:{}) forwards to module function '{}', which forwards to '{}.{}' ({}:{}) — \
                 dead indirection: C already depends on {}'s module, so {} could reach '{}' directly — collapse \
                 the chain — fix: collapse-chain",
                head.class,
                head.name,
                head.params.join(", "),
                head.rel,
                head.line,
                mid.name,
                d.class,
                d.name,
                d.rel,
                d.line,
                d.class,
                head.class,
                d.name
            )
        } else {
            // terminal is a module fn: M -> F -> G at depth >= 2 (#27 handoff)
            let g = &mid.callee["Fn:".len()..];
            if !depends_on(&head.rel, &mid.rel, imports_by_rel) {
                continue;
            }
            format!(
                "method '{}.{}({})' ({}:{}) forwards to module function '{}', which forwards to module function \
                 '{}' — dead indirection at depth >= 2 — collapse the chain — fix: collapse-chain",
                head.class,
                head.name,
                head.params.join(", "),
                head.rel,
                head.line,
                mid.name,
                g
            )
        };
        out.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col: 0,
            file: head.rel.clone(),
            line: head.line,
            function: head.name.clone(),
            kind: "forwarding-chain".into(),
            severity: "fail".into(),
            message,
        });
    }
    out
}
// ------------------------------------------------------------- #28 process-class

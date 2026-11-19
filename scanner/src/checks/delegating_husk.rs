//! `delegating-husk`: a class whose every method forwards to the same object —
//! it adds a name, not a behaviour.

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt, StmtFunctionDef};

use crate::finding::Finding;
use crate::{stmt_line, ScanState};

use super::class_facts::class_methods;
use super::function_facts::FunctionFacts;

/// Is `body` exactly `return <callee>(<params>)` with EVERY param passed
/// positionally in order — the pure-forwarder SHAPE shared by
/// delegating-husk and forwarding-chain (#30). A Name callee receives all
/// params; an Attribute callee (a method on another object) receives the
/// receiver as its leading param and the REST as call args
/// (`def get_stored(store, key): return store.load(key)`).
fn is_forwarder_shaped(body: &[Stmt], params: &[String]) -> bool {
    if body.len() != 1 {
        return false;
    }
    let Stmt::Return(r) = &body[0] else { return false };
    let Some(value) = r.value.as_deref() else { return false };
    let Expr::Call(c) = value else { return false };
    if !c.arguments.keywords.is_empty() {
        return false;
    }
    let expected: &[String] = match c.func.as_ref() {
        Expr::Name(_) => params,
        Expr::Attribute(a) => {
            // receiver param MUST be the value's root name
            let Expr::Name(recv) = a.value.as_ref() else {
                return false;
            };
            let Some(first) = params.first() else { return false };
            if recv.id.as_str() != first.as_str() {
                return false;
            }
            &params[1..]
        }
        _ => return false,
    };
    if c.arguments.args.len() != expected.len() {
        return false;
    }
    for (arg, param) in c.arguments.args.iter().zip(expected) {
        let Expr::Name(an) = arg else { return false };
        if an.id.as_str() != param.as_str() {
            return false;
        }
    }
    true
}

/// Class where every instance method's body is exactly
/// `return F(<all params>)` with F a module-level function — an indirection
/// layer and no behavior (#27 delegating-husk). Depth >=2 chains
/// (M -> F -> G, F itself a pure forwarder) are #30's shape — hands off.
pub fn delegating_husk_findings(state: &mut ScanState, body: &[Stmt]) {
    let module_fns: HashSet<String> = body
        .iter()
        .filter_map(|s| match s {
            Stmt::FunctionDef(f) => Some(f.name.to_string()),
            _ => None,
        })
        .collect();
    if module_fns.is_empty() {
        return;
    }
    // module fns that are THEMSELVES pure forwarders — a husk whose callee
    // forwards is the #30 chain shape, not a husk.
    let forwarder_fns: HashSet<String> = body
        .iter()
        .filter_map(|s| match s {
            Stmt::FunctionDef(f) => {
                let params = FunctionFacts::fn_param_names(f);
                if is_forwarder_shaped(&f.body, &params) {
                    Some(f.name.to_string())
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    let method_params = |m: &StmtFunctionDef| {
        FunctionFacts::fn_param_names(m)
            .into_iter()
            .filter(|p| p != "self" && p != "cls")
            .collect::<Vec<_>>()
    };
    for s in body {
        let Stmt::ClassDef(cls) = s else { continue };
        let methods = class_methods(cls);
        if methods.is_empty() {
            continue;
        }
        if methods.iter().any(|m| {
            let mut callee = String::new();
            let params = method_params(m);
            !FunctionFacts::is_pure_forwarder_body(&m.body, &mut callee, &params) || !module_fns.contains(&callee)
        }) {
            continue;
        }
        // every method forwards to a module fn; none of those fns forwards
        // itself (the depth-2 shape belongs to forwarding-chain)
        let mut chains_down = false;
        for m in &methods {
            let params = method_params(m);
            let mut callee = String::new();
            if FunctionFacts::is_pure_forwarder_body(&m.body, &mut callee, &params) && forwarder_fns.contains(&callee) {
                chains_down = true;
            }
        }
        if chains_down {
            continue;
        }
        let line = stmt_line(state.source, s);
        state.findings.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col: 0,
            file: state.file.to_string(),
            line,
            function: cls.name.to_string(),
            kind: "delegating-husk".into(),
            severity: "warn".into(),
            message: format!(
                "class '{}' only forwards to module functions — every method is exactly `return F(<all params>)` \
             with no behavior of its own: callers could reach the functions directly, and the class name is a \
             namespace for others' work. Dissolve the husk — rewire its callers to the module functions and \
             delete the class; NEVER inline the functions' bodies into it — fix: dissolve-husk",
                cls.name.as_str()
            ),
        });
    }
}
// ------------------------------------------------------------- #30 forwarding-chain

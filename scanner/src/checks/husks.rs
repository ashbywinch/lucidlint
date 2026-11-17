//! The husk families: a class with no state of its own (`static-husk`) and a
//! class whose every method forwards (`delegating-husk`).

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt, StmtClassDef, StmtFunctionDef};

use crate::finding::Finding;

use super::{annotation_base_name, is_static_husk, push_stmt_children};
use crate::{stmt_line, ScanState};

fn is_self_attr_expr(e: &Expr) -> bool {
    matches!(e, Expr::Attribute(a) if matches!(a.value.as_ref(), Expr::Name(n) if n.id.as_str() == "self"))
}

/// Does any statement in `body` write `self.<attr>` (assign/annassign/
/// augassign)? The static-husk rule's "no state of its own" test.
fn body_writes_self_attr(body: &[Stmt]) -> bool {
    let mut stack: Vec<&Stmt> = body.iter().collect();
    while let Some(s) = stack.pop() {
        match s {
            Stmt::Assign(a) => {
                if a.targets.iter().any(is_self_attr_expr) {
                    return true;
                }
            }
            Stmt::AnnAssign(a) => {
                if is_self_attr_expr(a.target.as_ref()) {
                    return true;
                }
            }
            Stmt::AugAssign(a) if is_self_attr_expr(a.target.as_ref()) => {
                return true;
            }
            _ => {}
        }
        push_stmt_children(s, &mut stack);
    }
    false
}

pub(super) fn class_methods(cls: &StmtClassDef) -> Vec<&StmtFunctionDef> {
    cls.body
        .iter()
        .filter_map(|m| match m {
            Stmt::FunctionDef(f) => Some(f),
            _ => None,
        })
        .collect()
}

pub(super) fn has_decorator(f: &StmtFunctionDef, name: &str) -> bool {
    f.decorator_list.iter().any(|d| match &d.expression {
        Expr::Name(n) => n.id.as_str() == name,
        Expr::Attribute(a) => a.attr.as_str() == name,
        _ => false,
    })
}

/// Class with NO state of its own (no `self.X =` anywhere, no dataclass
/// fields, no properties) whose members are ALL staticmethods — a namespace
/// wearing a domain noun (#27 static-husk). Exclusions: inherited base (state
/// may live in the base), ABC/Protocol (pure interfaces), pass-only subclass,
/// `*Error`/`*Exception` (exception namespace).
pub fn static_husk_findings(state: &mut ScanState, body: &[Stmt]) {
    for s in body {
        let Stmt::ClassDef(cls) = s else { continue };
        let has_base = cls.arguments.as_ref().is_some_and(|a| !a.args.is_empty());
        if has_base {
            continue; // inherited base — the state may live in the base
        }
        let name = cls.name.as_str();
        if name.ends_with("Error") || name.ends_with("Exception") {
            continue; // exception namespace
        }
        let methods = class_methods(cls);
        if methods.is_empty() {
            continue; // pass-only subclass / re-export marker
        }
        let abstract_ = methods.iter().any(|m| has_decorator(m, "abstractmethod"));
        let is_abc = cls.arguments.as_ref().is_some_and(|a| {
            a.args
                .iter()
                .any(|b| annotation_base_name(b).is_some_and(|n| n == "ABC" || n == "Protocol"))
        });
        if abstract_ || is_abc {
            continue; // pure interface
        }
        let has_property = methods.iter().any(|m| has_decorator(m, "property"));
        let has_dataclass_fields = cls.body.iter().any(|m| matches!(m, Stmt::AnnAssign(_)))
            || cls.decorator_list.iter().any(|d| match &d.expression {
                Expr::Name(n) => n.id.as_str() == "dataclass",
                Expr::Attribute(a) => a.attr.as_str() == "dataclass",
                _ => false,
            });
        let writes_state = methods.iter().any(|m| body_writes_self_attr(&m.body));
        let all_static = methods.iter().all(|m| has_decorator(m, "staticmethod"));
        if !is_static_husk(all_static, has_property, has_dataclass_fields, writes_state) {
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
            kind: "static-husk".into(),
            severity: "warn".into(),
            message: format!(
                "Class {} has no state of its own and only staticmethods — a namespace wearing a domain noun. \
             The fix: find the state these operations work on and put it IN the class (fields + method bodies \
             reading them); if there is no such state, the operations still belong with SOME abstraction or \
             another — possibly more than one: the class(es) that own the state they work on. Finding it is \
             the task; reconsider whether THIS class is the correct abstraction. The methods stop being static: \
             give the class a constructor that takes the state, instance attributes that hold it, and instance \
             methods that read it.",
                name
            ),
        });
    }
}

/// The names a function's parameter list binds (positional, keyword-only,
/// vararg, kwarg) in order.
pub(super) fn fn_param_names(f: &StmtFunctionDef) -> Vec<String> {
    let mut out = Vec::new();
    for p in f.parameters.posonlyargs.iter().chain(&f.parameters.args) {
        out.push(p.parameter.name.to_string());
    }
    for p in &f.parameters.kwonlyargs {
        out.push(p.parameter.name.to_string());
    }
    if let Some(v) = &f.parameters.vararg {
        out.push(v.name.to_string());
    }
    if let Some(k) = &f.parameters.kwarg {
        out.push(k.name.to_string());
    }
    out
}

/// Is `body` exactly `return F(<all params positionally in order>)`? The
/// forwarder shape shared by delegating-husk and forwarding-chain.
pub(super) fn is_pure_forwarder_body(body: &[Stmt], callee: &mut String, params: &[String]) -> bool {
    if body.len() != 1 {
        return false;
    }
    let Stmt::Return(r) = &body[0] else { return false };
    let Some(value) = r.value.as_deref() else { return false };
    let Expr::Call(c) = value else { return false };
    let Expr::Name(n) = c.func.as_ref() else { return false };
    if c.arguments.args.len() != params.len() || !c.arguments.keywords.is_empty() {
        return false;
    }
    for (arg, param) in c.arguments.args.iter().zip(params) {
        let Expr::Name(an) = arg else { return false };
        if an.id.as_str() != param.as_str() {
            return false;
        }
    }
    callee.clear();
    callee.push_str(n.id.as_str());
    true
}

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
                let params = fn_param_names(f);
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
        fn_param_names(m)
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
            !is_pure_forwarder_body(&m.body, &mut callee, &params) || !module_fns.contains(&callee)
        }) {
            continue;
        }
        // every method forwards to a module fn; none of those fns forwards
        // itself (the depth-2 shape belongs to forwarding-chain)
        let mut chains_down = false;
        for m in &methods {
            let params = method_params(m);
            let mut callee = String::new();
            if is_pure_forwarder_body(&m.body, &mut callee, &params) && forwarder_fns.contains(&callee) {
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

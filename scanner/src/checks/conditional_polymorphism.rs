//! `conditional-polymorphism`: the same type test repeated in branches — the
//! branches are subclasses waiting to be named.

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::{line_of, ScanState};

use super::expr_walk::base_name_of;

/// The dispatch-key (left side) of a chain-arm test — the value all arms
/// compare. None when the test is not a simple comparison on one value.
pub(crate) fn dispatch_key(e: &Expr) -> Option<String> {
    match e {
        Expr::Compare(c) => match c.left.as_ref() {
            Expr::Name(n) => Some(n.id.to_string()),
            Expr::Attribute(a) => Some(format!("{}.{}", base_name_of(&a.value)?, a.attr)),
            _ => None,
        },
        Expr::Call(c) => {
            // isinstance(x, A) dispatches on x — conditional-polymorphism
            // groups chains by the VALUE they discriminate on (the
            // latent-visitor family keys by the dispatched TYPE instead,
            // see dispatch_arm)
            if let Expr::Name(n) = c.func.as_ref() {
                if n.id.as_str() == "isinstance" {
                    if let Some(Expr::Name(t)) = c.arguments.args.first() {
                        return Some(format!("isinstance({})", t.id));
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Replace Conditional with Polymorphism: an if/elif chain of >= 4 arms whose
/// tests all dispatch on the same value — the type-tag conditional.
pub fn conditional_polymorphism_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    fn walk(state: &mut ScanState, stmts: &[Stmt], source: &str) {
        for s in stmts {
            if let Stmt::If(i) = s {
                let arms: Vec<&Expr> = std::iter::once(i.test.as_ref())
                    .chain(i.elif_else_clauses.iter().filter_map(|c| c.test.as_ref()))
                    .collect();
                let chain_line = line_of(source, s.range().start());
                if arms.len() >= 4 && !state.claimed_dispatch.contains(&chain_line) {
                    let keys: Vec<Option<String>> = arms.iter().map(|t| dispatch_key(t)).collect();
                    if keys.iter().all(|k| k.is_some()) && keys.windows(2).all(|w| w[0] == w[1]) {
                        let line = chain_line;
                        state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                                    file: state.file.to_string(),
                                                    line,
                                                    function: state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default(),
                                                    kind: "conditional-polymorphism".into(),
                                                    severity: "warn".into(),
                                                    message: format!(
                                                        "{} arms dispatch on the same value ('{}') — Replace Conditional with Polymorphism: one method per case",
                                                        arms.len(),
                                                        keys[0].as_deref().unwrap_or("")
                                                    ), });
                    }
                }
                walk(state, &i.body, source);
                for cl in &i.elif_else_clauses {
                    walk(state, &cl.body, source);
                }
            } else {
                match s {
                    Stmt::FunctionDef(f) => walk(state, &f.body, source),
                    Stmt::ClassDef(cd) => walk(state, &cd.body, source),
                    Stmt::For(f) => walk(state, &f.body, source),
                    Stmt::While(w) => walk(state, &w.body, source),
                    Stmt::Try(t) => {
                        walk(state, &t.body, source);
                        for h in &t.handlers {
                            let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                            walk(state, &eh.body, source);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    walk(state, body, source);
}

// ---------------------------------------------------------- scattered-instance

// =====================================================================
// over-abstraction: an ABC with exactly one concrete subclass is ceremony
// (the last Python finding family — cross-file class + import resolution)
// =====================================================================

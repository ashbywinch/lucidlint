//! The except family: swallows (fail) and broad excepts (warn).

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

use super::super::class_facts::annotation_base_name;
use super::super::expr_walk::{push_stmt_children, stmt_exprs};
use super::super::naming_units::NamingUnits;

/// The except rule: a handler that surfaces nothing, or a boundary that claims
/// too much.
pub struct Except;

impl Except {
    /// Report every swallowing handler and every broad `except Exception`.
    pub fn findings(state: &mut ScanState, stmt: &Stmt) {
        let Stmt::Try(t) = stmt else { return };
        let returned: HashSet<String> = state.fn_stack.last().map(|s| s.returned.clone()).unwrap_or_default();
        let fn_name = state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        for handler in &t.handlers {
            let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = handler;
            let type_opt = eh.type_.as_ref();
            let swallows = match type_opt {
                None => true, // bare except
                Some(_) => {
                    let body: Vec<&Stmt> = eh.body.iter().collect();
                    !Self::handler_exits(&body, &returned)
                }
            };
            if swallows {
                let line = Self::header_colon_line(state.source, eh);
                let kind = if type_opt.is_none() {
                    "bare except"
                } else {
                    "except that swallows"
                };
                state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                file: state.file.to_string(),
                                line,
                                function: fn_name.clone(),
                                kind: "swallow".into(),
                                severity: "fail".into(),
                                message: format!(
                                    "{kind} at line {line} — logs are not surfacing: a caller exists that needs to decide; surface by return, raise, break, continue, sys.exit, or mutating a name the enclosing function returns"
                                ), });
            } else if let Some(ty) = type_opt {
                let base = annotation_base_name(ty);
                if matches!(base.as_deref(), Some("Exception") | Some("BaseException")) {
                    state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                        file: state.file.to_string(),
                                        line: Self::header_colon_line(state.source, eh),
                                        function: fn_name.clone(),
                                        kind: "broad-except".into(),
                                        severity: "warn".into(),
                                        message: "broad except Exception - catch what you actually handle; a true boundary catch states its blast radius in the why".into(), });
                }
            }
        }
    }

    /// The line the except header's colon sits on — the line a trailing marker
    /// on the except clause rides. Single-line headers: the `except` line. A
    /// multi-line `except (…):` header: the closing `):` line (plan Phase 3:
    /// the server.py:704 shape — a marker on the except's own line binds and
    /// clears; the finding anchors where its trailing marker would sit).
    fn header_colon_line(source: &str, eh: &ruff_python_ast::ExceptHandlerExceptHandler) -> usize {
        let bytes = source.as_bytes();
        let mut i = eh.range().start().to_usize();
        let end = eh.range().end().to_usize();
        let mut depth = 0usize;
        while i < end {
            match bytes[i] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b':' if depth == 0 => {
                    return Source::line_of(source, ruff_text_size::TextSize::new(i as u32));
                }
                _ => {}
            }
            i += 1;
        }
        Source::line_of(source, eh.range().start())
    }

    /// A handler surfaces the error when it exits with control flow, calls
    /// sys.exit/exit/quit, or mutates a name the enclosing function returns.
    fn handler_exits(handler_body: &[&Stmt], returned: &HashSet<String>) -> bool {
        let mut exits = false;
        let mut walks = handler_body.to_vec();
        while let Some(stmt) = walks.pop() {
            match stmt {
                Stmt::Return(_) | Stmt::Raise(_) | Stmt::Break(_) | Stmt::Continue(_) => return true,
                Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {
                    // Nested fn/class bodies can't exit the handler — `return`
                    // inside a nested fn returns from THAT fn, not the handler.
                    continue;
                }
                _ => {}
            }
            let mut process_exit = false;
            Self::walk_handler(stmt, &mut exits, &mut process_exit, returned);
            if exits || process_exit {
                return true; // surfaced by control flow or sys.exit(...)
            }
        }
        false
    }

    fn walk_handler(stmt: &Stmt, exits: &mut bool, process_exit: &mut bool, returned: &HashSet<String>) {
        // process exit / returned-name mutation detection via a manual walk
        let mut stack: Vec<&Stmt> = vec![stmt];
        while let Some(s) = stack.pop() {
            if matches!(s, Stmt::Return(_) | Stmt::Raise(_) | Stmt::Break(_) | Stmt::Continue(_)) {
                *exits = true;
                return;
            }
            if matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
                // Nested fn/class bodies can't exit the handler.
                continue;
            }
            // store-target mutation of a returned name surfaces the error
            if Self::mutates_returned_target(s, returned) {
                *exits = true;
                return;
            }
            for expr in stmt_exprs(s) {
                if Self::is_process_exit(expr) {
                    *process_exit = true;
                }
                if let Expr::Call(c) = expr {
                    if let Expr::Attribute(attr) = c.func.as_ref() {
                        if let Expr::Name(n) = attr.value.as_ref() {
                            if returned.contains(n.id.as_str()) {
                                *exits = true;
                                return;
                            }
                        }
                    }
                }
            }
            // descend into nested stmt bodies (except function bodies)
            push_stmt_children(s, &mut stack);
        }
    }

    fn is_process_exit(e: &Expr) -> bool {
        match e {
            Expr::Call(c) => match c.func.as_ref() {
                Expr::Name(n) => matches!(n.id.as_str(), "exit" | "quit"),
                Expr::Attribute(a) => {
                    matches!(a.attr.as_str(), "exit")
                        && matches!(a.value.as_ref(), Expr::Name(n) if n.id.as_str() == "sys")
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn mutates_returned_target(s: &Stmt, returned: &HashSet<String>) -> bool {
        let targets: Vec<&Expr> = match s {
            Stmt::Assign(a) => a.targets.iter().collect(),
            Stmt::AugAssign(a) => vec![a.target.as_ref()],
            Stmt::AnnAssign(a) => vec![a.target.as_ref()],
            Stmt::Delete(d) => d.targets.iter().collect(),
            _ => return false,
        };
        for t in targets {
            match t {
                Expr::Name(n) if returned.contains(n.id.as_str()) => return true,
                Expr::Subscript(sub) if returned.contains(NamingUnits::sub_name(sub).as_str()) => return true,
                _ => {}
            }
        }
        false
    }
}

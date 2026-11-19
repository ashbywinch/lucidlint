//! Signature hygiene: a boolean literal passed positionally, a list of
//! same-kind literals, a method detached from its object, an over-long parameter
//! list, and an except that swallows. Each makes the call site unreadable.

use std::collections::HashSet;

use ruff_python_ast::{Expr, ExprCall, Stmt, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::finding::Finding;

use super::builtin_shadow::SHADOWED_BUILTINS;
use super::class_facts::annotation_base_name;
use super::expr_walk::{push_stmt_children, stmt_exprs};
use super::function_facts::body_refs_name;
use super::naming_units::NamingUnits;
use crate::{line_of, ScanState};

/// A positional call argument that is a literal True/False — the intent is
/// unreadable at the call site; it should be a named keyword
/// (f(..., retry=True)). Keyword arguments are self-documenting and exempt.
///
/// Lookup-with-default calls are exempt: the trailing boolean IS the default
/// for a missing key/attribute (`d.get("retryable", False)`), and those
/// parameters are positional-only — the "name it" prescription is
/// impossible, so the finding would be unactionable noise.
pub fn boolean_arg_findings(state: &mut ScanState, call: &ExprCall, source: &str) {
    let fn_name = state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
    // only ATTRIBUTE receivers (cfg.get, os.environ.get) and the bare
    // getattr builtin are lookup-with-default calls — a user function
    // literally named `get`/`setdefault`/`pop` is not, and must stay
    // flagged (review bot)
    let bare_lookup = matches!(call.func.as_ref(), Expr::Name(n) if n.id.as_str() == "getattr");
    let attr_lookup =
        matches!(call.func.as_ref(), Expr::Attribute(a) if LOOKUP_DEFAULT_CALLEES.contains(&a.attr.as_str()));
    let lookup_default = bare_lookup || attr_lookup;
    for (i, arg) in call.arguments.args.iter().enumerate() {
        if let Expr::BooleanLiteral(_) = arg {
            // the trailing positional boolean on a lookup call is the
            // DEFAULT, not a flag — positional-only, cannot be keyworded
            if lookup_default && i + 1 == call.arguments.args.len() {
                continue;
            }
            state.findings.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                col: 0,
                file: state.file.to_string(),
                line: line_of(source, arg.range().start()),
                function: fn_name.clone(),
                kind: "boolean-arg".into(),
                severity: "fail".into(),
                message: "boolean literal argument — name it: f(..., retry=True)".to_string(),
            });
        }
    }
}

/// Callee names whose trailing positional boolean is a lookup DEFAULT, not a
/// flag — the parameter is positional-only in CPython, so the rule's own
/// "keyword it" prescription cannot apply.
const LOOKUP_DEFAULT_CALLEES: &[&str] = &["get", "getattr", "setdefault", "pop"];

/// Positional literals of the same kind — the classic argument-swapping bug
/// (`set_limits(10, 20)` — which is min, which is max?). Warn tier: not every
/// `f(1, 2)` is a defect (coordinates), but a keyword call eliminates the
/// class entirely. Builtin callees are exempt — their arities are fixed and
/// well-known (range, print, ...).
pub fn positional_literals_findings(state: &mut ScanState, call: &ExprCall, source: &str) {
    // Only PLAIN function calls (a Name callee, not a method): the builtin
    // methods that dominate real code (dict.get, str.replace, os.environ.get)
    // have canonical positional semantics — keywords are impossible or never
    // swapped, and flagging them is pure noise. User-defined functions have
    // no such convention, so a same-kind literal pair there is a real swap
    // risk. Builtin NAMES (range/print/min/max) are fixed-arities — exempt.
    let Expr::Name(n) = call.func.as_ref() else {
        return;
    };
    if SHADOWED_BUILTINS.contains(&n.id.as_str()) {
        return;
    }
    let callee_name = n.id.to_string();
    let mut ints = 0usize;
    let mut floats = 0usize;
    let mut strings = 0usize;
    for arg in &call.arguments.args {
        match arg {
            Expr::NumberLiteral(n) => match n.value {
                ruff_python_ast::Number::Int(_) => ints += 1,
                ruff_python_ast::Number::Float(_) => floats += 1,
                ruff_python_ast::Number::Complex { .. } => {}
            },
            Expr::StringLiteral(_) => strings += 1,
            _ => {}
        }
    }
    let (n, kind) = if ints >= 2 {
        (ints, "numbers")
    } else if floats >= 2 {
        (floats, "floats")
    } else if strings >= 2 {
        (strings, "strings")
    } else {
        return;
    };
    let fn_name = state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
    // The directive states THIS site's case — no conditional for the reader
    // to evaluate (user ruling: never suggest a fix that cannot fix). A
    // same-file callee resolves from the call's file alone, so the bare
    // command fixes; anything else carries its semantic slot up front, the
    // same contract as magic-number's --name <CONST>. A call textually
    // above its def misclassifies toward the slot — an unnecessary
    // placeholder, never a command that cannot fix.
    let directive = if state.defs.iter().any(|(name, _)| *name == callee_name) {
        " — fix: positional-literals"
    } else {
        " — fix: positional-literals --params <names>"
    };
    state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
            file: state.file.to_string(),
            line: line_of(source, call.range().start()),
            function: fn_name,
            kind: "positional-literals".into(),
            severity: "warn".into(),
            message: format!(
                "call passes {n} {kind} positionally to {callee}() — a swapped argument is a silent bug; use keyword arguments{directive}",
                callee = callee_name,
            ), });
}

/// A method whose body never references its receiver — it does not touch
/// instance state, so it does not belong in the class (the inverse of
/// record-shape/strewing). `@classmethod`/`@staticmethod` are explicit
/// non-instance methods — exempt by design.
pub fn detached_method_findings(state: &mut ScanState, f: &StmtFunctionDef, source: &str) {
    let first = f.parameters.posonlyargs.first().or_else(|| f.parameters.args.first());
    let Some(receiver) = first else {
        return;
    };
    let recv = receiver.parameter.name.id.as_str();
    let decorated_class_level = f.decorator_list.iter().any(
        |d| matches!(&d.expression, Expr::Name(n) if n.id.as_str() == "classmethod" || n.id.as_str() == "staticmethod"),
    );
    if decorated_class_level {
        return;
    }
    // __init__/__new__ can never be staticmethods
    if f.name.as_str() == "__init__" || f.name.as_str() == "__new__" {
        return;
    }
    // a trivial stub (`...`, `pass`, docstring, `return None`, a lone raise)
    // is a protocol/interface placeholder — the binding is the interface's
    // contract, not a local judgment call (long-param-list's stub rationale;
    // houses: CommuteRouterLike.get_commute)
    if is_trivial_stub(&f.body) {
        return;
    }
    // super() needs the binding even when the body never names the receiver
    // (super().__init__(v) carries no literal self)
    if body_refs_name(&f.body, "super") {
        return;
    }
    // an override's binding is the base class's contract, not a local call
    if f.decorator_list.iter().any(|d| {
        matches!(&d.expression, Expr::Name(n) if n.id.as_str() == "override")
            || matches!(&d.expression, Expr::Attribute(a) if a.attr.as_str() == "override")
    }) {
        return;
    }
    if body_refs_name(&f.body, recv) {
        return;
    }
    let fn_name = f.name.to_string();
    let line = line_of(source, f.name.range().start());
    state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
            file: state.file.to_string(),
            line,
            function: fn_name.clone(),
            kind: "detached-method".into(),
            severity: "warn".into(),
            message: format!(
                "method '{fn_name}' never uses '{recv}' — it does not touch instance state; make it a @staticmethod or move it out of the class"
            ), });
}

/// A def with more than 5 parameters (a leading self/cls excluded — that is
/// convention, not a parameter) — the signature is doing too much. A trivial
/// stub is exempt: a one-statement body (empty, `pass`, `return None`) is a
/// framework override placeholder — the arity is the protocol's, and a
/// parameter object would BREAK the override (review: urllib's
/// redirect_request).
pub fn long_param_list_findings(state: &mut ScanState, f: &StmtFunctionDef, source: &str) {
    let mut n = f.parameters.posonlyargs.len() + f.parameters.args.len() + f.parameters.kwonlyargs.len();
    if let Some(first) = f.parameters.posonlyargs.first().or_else(|| f.parameters.args.first()) {
        let name = first.parameter.name.as_str();
        if name == "self" || name == "cls" {
            n -= 1;
        }
    }
    if n > 5 && !is_trivial_stub(&f.body) {
        state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
        file: state.file.to_string(),
        line: line_of(source, f.name.range().start()),
        function: f.name.to_string(),
        kind: "long-param-list".into(),
        severity: "fail".into(),
        message: format!(
            "{n} parameters — the call site cannot see what each value means, and no name says what they are together. Group the parameters that belong together into one object named with a domain noun. — fix: long-param-list --fix-name <Options>"
        ), });
    }
}

/// A one-statement placeholder body: `pass`, a bare expression, a lone
/// `raise`, or a `return` of nothing / None. A 6-param function that does
/// nothing is a protocol stub, not a param-list smell; a stub method never
/// touches instance state by design.
fn is_trivial_stub(body: &[Stmt]) -> bool {
    if body.len() > 1 {
        return false;
    }
    match body.first() {
        None => true,
        Some(Stmt::Pass(_)) => true,
        Some(Stmt::Expr(_)) => true,
        Some(Stmt::Raise(_)) => true,
        Some(Stmt::Return(r)) => match r.value.as_ref() {
            None => true,
            Some(e) => matches!(e.as_ref(), Expr::NoneLiteral(_)),
        },
        _ => false,
    }
}

/// The line the except header's colon sits on — the line a trailing marker
/// on the except clause rides. Single-line headers: the `except` line. A
/// multi-line `except (…):` header: the closing `):` line (plan Phase 3:
/// the server.py:704 shape — a marker on the except's own line binds and
/// clears; the finding anchors where its trailing marker would sit).
fn except_header_colon_line(source: &str, eh: &ruff_python_ast::ExceptHandlerExceptHandler) -> usize {
    let bytes = source.as_bytes();
    let mut i = eh.range().start().to_usize();
    let end = eh.range().end().to_usize();
    let mut depth = 0usize;
    while i < end {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b':' if depth == 0 => {
                return line_of(source, ruff_text_size::TextSize::new(i as u32));
            }
            _ => {}
        }
        i += 1;
    }
    line_of(source, eh.range().start())
}

/// The except family: swallows (fail) and broad excepts (warn).
pub fn except_findings(state: &mut ScanState, stmt: &Stmt) {
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
                !handler_exits(&body, &returned)
            }
        };
        if swallows {
            let line = except_header_colon_line(state.source, eh);
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
                                    line: except_header_colon_line(state.source, eh),
                                    function: fn_name.clone(),
                                    kind: "broad-except".into(),
                                    severity: "warn".into(),
                                    message: "broad except Exception - catch what you actually handle; a true boundary catch states its blast radius in the why".into(), });
            }
        }
    }
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
        walk_handler(stmt, &mut exits, &mut process_exit, returned);
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
        if mutates_returned_target(s, returned) {
            *exits = true;
            return;
        }
        for expr in stmt_exprs(s) {
            if is_process_exit(expr) {
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
                matches!(a.attr.as_str(), "exit") && matches!(a.value.as_ref(), Expr::Name(n) if n.id.as_str() == "sys")
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

// code-health: ignore-file complexity the parity-locked AST walkers are single dispatch tables —
// match-arm count is table size, not branching; keep NEW functions under cc 15

//! The remaining standard-family checks, mirroring the Python implementation
//! exactly: suppressions, type-ignore, global-state, builtin-shadow, closures,
//! class-module, vague-name, strewing, except-swallows, broad-except.

use ruff_python_ast::token::{TokenKind, Tokens};
use ruff_python_ast::{
    AnyNodeRef, BoolOp, CmpOp, Decorator, Expr, ExprAttribute, ExprContext, Operator, Pattern, Stmt, StmtClassDef,
    StmtFunctionDef, UnaryOp,
};
use ruff_text_size::Ranged;
use std::collections::HashSet;

use crate::{line_of, stmt_line, Finding, ScanState};

pub use crate::common::VAGUE_SUFFIXES;

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

/// Comment tokens -> (line, comment text incl. '#') — from the parsed
/// token stream (0.0.9's lexer exposes no public range accessor).
pub fn comment_lines(source: &str, tokens: &Tokens) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for tok in tokens.iter() {
        if tok.kind() == TokenKind::Comment {
            let range = tok.range();
            let text = &source[range];
            out.push((line_of(source, range.start()), text.to_string()));
        }
    }
    out
}

/// Python comment tokens -> the shared suppression map (`#` comments).
pub fn parse_suppressions(source: &str, tokens: &Tokens) -> crate::common::Suppressions {
    crate::common::suppressions_from_comments(&comment_lines(source, tokens))
}

/// Filter findings through the suppressions + emit the why-less suppression
/// findings — shared logic over this language's comment lines (the parse
/// itself lives in `common::suppressions_from_comments`). `pre_used` carries
/// the suppressions the cc-retain already honored (complexity suppressions
/// are applied before the findings filter; stale detection must not re-flag
/// them).
pub fn apply_suppressions_impl(
    findings: Vec<Finding>,
    source: &str,
    file: &str,
    tokens: &Tokens,
    pre_used: &crate::common::PreUsedSuppressions,
) -> Vec<Finding> {
    crate::common::apply_suppressions_impl(findings, &comment_lines(source, tokens), file, "#", pre_used)
}

/// `# type: ignore` without a why (a second comment on the line) is a finding.
pub fn type_ignore_findings(source: &str, file: &str, tokens: &Tokens) -> Vec<Finding> {
    let mut out = Vec::new();
    for (ln, text) in comment_lines(source, tokens) {
        if !text.contains("type: ignore") {
            continue;
        }
        let rest = text.split_once("type: ignore").map(|(_, r)| r).unwrap_or("");
        if !rest.contains('#') {
            out.push(Finding {
                file: file.to_string(),
                line: ln,
                function: String::new(),
                kind: "type-ignore".into(),
                severity: "fail".into(),
                message: format!(
                    "# type: ignore at line {ln} without a why — a suppression is itself a finding: add a comment explaining why the checker is wrong"
                ),
            });
        }
    }
    out
}

/// `# noqa` / `# pragma: no cover` without a why (a second comment on the
/// line) is a finding — the same why-detection heuristic as
/// `type_ignore_findings`, over the same comment-token stream.
pub fn noqa_findings(source: &str, file: &str, tokens: &Tokens) -> Vec<Finding> {
    let mut out = Vec::new();
    for (ln, text) in comment_lines(source, tokens) {
        let rest = if text.contains("# noqa") {
            text.split_once("# noqa").map(|(_, r)| r).unwrap_or("")
        } else if text.contains("# pragma: no cover") {
            text.split_once("# pragma: no cover").map(|(_, r)| r).unwrap_or("")
        } else {
            continue;
        };
        if !rest.contains('#') {
            out.push(Finding {
                file: file.to_string(),
                line: ln,
                function: String::new(),
                kind: "noqa".into(),
                severity: "fail".into(),
                message: "# noqa without a why — explain what the checker gets wrong".to_string(),
            });
        }
    }
    out
}

/// Module-level mutable literals, Global statements, and mutations of module
/// containers inside functions — mirrors the dispatcher's global-state handlers.
pub fn global_state_findings(state: &mut ScanState, stmt: &Stmt, module_level: bool) {
    let fn_name = state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
    if let Stmt::Global(g) = stmt {
        let line = stmt_line(state.source, stmt);
        state.findings.push(Finding {
            file: state.file.to_string(),
            line,
            function: fn_name.clone(),
            kind: "global-state".into(),
            severity: "fail".into(),
            message: format!(
                "global statement at line {line} — no module-level mutable state: pass it around or keep ONE global services object set at the entry point"
            ),
        });
        let _ = g;
        return;
    }
    if module_level {
        // both Assign and AnnAssign: typed dicts like `_oauth_states: dict = {}`
        // are module state exactly like bare literals (the Python
        // _module_literal_findings handles both via _module_assignment_target)
        let value = match stmt {
            Stmt::Assign(a) => Some(a.value.as_ref()),
            Stmt::AnnAssign(a) => a.value.as_deref(),
            _ => None,
        };
        let targets: Vec<&Expr> = match stmt {
            Stmt::Assign(a) => a.targets.iter().collect(),
            Stmt::AnnAssign(a) => vec![a.target.as_ref()],
            _ => Vec::new(),
        };
        if let Some(v) = value {
            if matches!(v, Expr::List(_) | Expr::Dict(_) | Expr::Set(_)) && !all_constant(v) {
                for target in targets {
                    if let Expr::Name(n) = target {
                        let line = stmt_line(state.source, stmt);
                        state.findings.push(Finding {
                            file: state.file.to_string(),
                            line,
                            function: fn_name.clone(),
                            kind: "global-state".into(),
                            severity: "fail".into(),
                            message: format!(
                                "module-level mutable collection '{}' — no module-level mutable state",
                                n.id.as_str()
                            ),
                        });
                    }
                }
            }
        }
    }
}

fn all_constant(e: &Expr) -> bool {
    match e {
        Expr::NumberLiteral(_)
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_) => true,
        Expr::UnaryOp(u) => all_constant(&u.operand), // -4.0 is a literal
        Expr::List(l) => !l.elts.is_empty() && l.elts.iter().all(all_constant),
        Expr::Set(s) => !s.elts.is_empty() && s.elts.iter().all(all_constant),
        Expr::Tuple(t) => t.elts.iter().all(all_constant),
        Expr::Dict(d) => {
            !d.items.is_empty()
                && d.items
                    .iter()
                    .all(|it| it.key.as_ref().map(all_constant).unwrap_or(false) && all_constant(&it.value))
        }
        _ => false,
    }
}

/// Parameters and local assignments shadowing builtins.
pub fn shadow_findings(state: &mut ScanState, stmt: &Stmt) {
    if let Stmt::FunctionDef(f) = stmt {
        for a in &f.parameters.posonlyargs {
            if SHADOWED_BUILTINS.contains(&a.parameter.name.as_str()) {
                state.findings.push(Finding {
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

/// A positional call argument that is a literal True/False — the intent is
/// unreadable at the call site; it should be a named keyword
/// (f(..., retry=True)). Keyword arguments are self-documenting and exempt.
pub fn boolean_arg_findings(state: &mut ScanState, args: &[Expr], source: &str) {
    let fn_name = state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
    for arg in args {
        if let Expr::BooleanLiteral(_) = arg {
            state.findings.push(Finding {
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

/// A def with more than 5 parameters (a leading self/cls excluded — that is
/// convention, not a parameter) — the signature is doing too much.
pub fn long_param_list_findings(state: &mut ScanState, f: &StmtFunctionDef, source: &str) {
    let mut n = f.parameters.posonlyargs.len() + f.parameters.args.len() + f.parameters.kwonlyargs.len();
    if let Some(first) = f.parameters.posonlyargs.first().or_else(|| f.parameters.args.first()) {
        let name = first.parameter.name.as_str();
        if name == "self" || name == "cls" {
            n -= 1;
        }
    }
    if n > 5 {
        state.findings.push(Finding {
            file: state.file.to_string(),
            line: line_of(source, f.name.range().start()),
            function: f.name.to_string(),
            kind: "long-param-list".into(),
            severity: "fail".into(),
            message: format!("{n} parameters — introduce a parameter object"),
        });
    }
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
            let line = line_of(state.source, eh.range().start());
            let kind = if type_opt.is_none() {
                "bare except"
            } else {
                "except that swallows"
            };
            state.findings.push(Finding {
                file: state.file.to_string(),
                line,
                function: fn_name.clone(),
                kind: "swallow".into(),
                severity: "fail".into(),
                message: format!(
                    "{kind} at line {line} — the catch never raises, returns, or surfaces the error; re-raise or mark `# code-health: ignore swallow <why>`"
                ),
            });
        } else if let Some(ty) = type_opt {
            let base = annotation_base_name(ty);
            if matches!(base.as_deref(), Some("Exception") | Some("BaseException")) {
                state.findings.push(Finding {
                    file: state.file.to_string(),
                    line: line_of(state.source, eh.range().start()),
                    function: fn_name.clone(),
                    kind: "broad-except".into(),
                    severity: "warn".into(),
                    message: "broad except Exception — catch the specific exception".into(),
                });
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

fn stmt_exprs(s: &Stmt) -> Vec<&Expr> {
    // the expressions directly reachable from a statement (one level)
    let mut out = Vec::new();
    match s {
        Stmt::Assign(a) => {
            push_expr_tree(a.value.as_ref(), &mut out);
            for t in &a.targets {
                push_expr_tree(t, &mut out);
            }
        }
        Stmt::AnnAssign(a) => {
            if let Some(v) = &a.value {
                push_expr_tree(v.as_ref(), &mut out);
            }
        }
        Stmt::AugAssign(a) => {
            push_expr_tree(a.value.as_ref(), &mut out);
            push_expr_tree(a.target.as_ref(), &mut out);
        }
        Stmt::Expr(e) => push_expr_tree(e.value.as_ref(), &mut out),
        Stmt::Return(r) => {
            if let Some(v) = &r.value {
                push_expr_tree(v.as_ref(), &mut out);
            }
        }
        Stmt::Raise(r) => {
            if let Some(e) = &r.exc {
                push_expr_tree(e.as_ref(), &mut out);
            }
        }
        Stmt::Assert(a) => {
            push_expr_tree(a.test.as_ref(), &mut out);
        }
        Stmt::If(i) => {
            push_expr_tree(i.test.as_ref(), &mut out);
        }
        Stmt::While(w) => {
            push_expr_tree(w.test.as_ref(), &mut out);
        }
        Stmt::For(f) => {
            push_expr_tree(f.target.as_ref(), &mut out);
            push_expr_tree(f.iter.as_ref(), &mut out);
        }
        Stmt::Delete(d) => {
            for t in &d.targets {
                push_expr_tree(t, &mut out);
            }
        }
        _ => {}
    }
    out
}

fn push_expr_tree<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    out.push(e);
    // descend one level for Call/Attribute/Subscript/BinOp shapes
    match e {
        Expr::Call(c) => {
            push_expr_tree(c.func.as_ref(), out);
            for a in &c.arguments.args {
                push_expr_tree(a, out);
            }
            for k in &c.arguments.keywords {
                push_expr_tree(&k.value, out);
            }
        }
        Expr::Attribute(a) => push_expr_tree(&a.value, out),
        Expr::Subscript(s) => {
            push_expr_tree(s.value.as_ref(), out);
            push_expr_tree(s.slice.as_ref(), out);
        }
        Expr::BinOp(b) => {
            push_expr_tree(b.left.as_ref(), out);
            push_expr_tree(b.right.as_ref(), out);
        }
        Expr::UnaryOp(u) => push_expr_tree(&u.operand, out),
        Expr::Compare(c) => {
            push_expr_tree(c.left.as_ref(), out);
            for o in &c.comparators {
                push_expr_tree(o, out);
            }
        }
        Expr::If(e) => {
            push_expr_tree(e.test.as_ref(), out);
            push_expr_tree(e.body.as_ref(), out);
            push_expr_tree(e.orelse.as_ref(), out);
        }
        _ => {}
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
            Expr::Subscript(sub) if returned.contains(sub_name(sub).as_str()) => return true,
            _ => {}
        }
    }
    false
}

fn push_stmt_children<'a>(s: &'a Stmt, stack: &mut Vec<&'a Stmt>) {
    match s {
        Stmt::If(i) => {
            for b in &i.body {
                stack.push(b);
            }
            for cl in &i.elif_else_clauses {
                for b in &cl.body {
                    stack.push(b);
                }
            }
        }
        Stmt::For(f) => {
            for b in &f.body {
                stack.push(b);
            }
            for b in &f.orelse {
                stack.push(b);
            }
        }
        Stmt::While(w) => {
            for b in &w.body {
                stack.push(b);
            }
            for b in &w.orelse {
                stack.push(b);
            }
        }
        Stmt::With(w) => {
            for b in &w.body {
                stack.push(b);
            }
        }
        Stmt::Try(t) => {
            for b in &t.body {
                stack.push(b);
            }
            for h in &t.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                for b in &eh.body {
                    stack.push(b);
                }
            }
            for b in &t.orelse {
                stack.push(b);
            }
            for b in &t.finalbody {
                stack.push(b);
            }
        }
        Stmt::Match(m) => {
            for case in &m.cases {
                for b in &case.body {
                    stack.push(b);
                }
            }
        }
        Stmt::ClassDef(c) => {
            for b in &c.body {
                stack.push(b);
            }
        }
        _ => {}
    }
}

/// Base name of an annotation/except-type expression: Name -> its id,
/// Attribute -> the attribute, anything else -> None (e.g. a tuple of types).
pub fn annotation_base_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => Some(a.attr.to_string()),
        _ => None,
    }
}

/// Closures: >= 2 inner functions/lambdas with cc >= 15 or span >= 60.
pub fn closure_findings(state: &mut ScanState, stmt: &Stmt, cc: u32, span: u32) {
    let Stmt::FunctionDef(f) = stmt else { return };
    let inner = inner_function_count(stmt);
    if inner < 2 {
        return;
    }
    if cc < 15 && span < 60 {
        return;
    }
    let line = stmt_line(state.source, stmt);
    state.findings.push(Finding {
        file: state.file.to_string(),
        line,
        function: f.name.to_string(),
        kind: "closures".into(),
        severity: "fail".into(),
        message: format!(
            "'{}' defines {inner} inner functions closing over its state — a class in disguise",
            f.name.as_str()
        ),
    });
}

pub fn inner_function_count(fn_stmt: &Stmt) -> u32 {
    let mut count = 0;
    let mut stack: Vec<&Stmt> = Vec::new();
    if let Stmt::FunctionDef(f) = fn_stmt {
        for s in &f.body {
            stack.push(s);
        }
    }
    while let Some(s) = stack.pop() {
        if let Stmt::FunctionDef(f) = s {
            count += 1;
            // descend into nested function bodies too — Python's ast.walk
            // counts functions at any depth
            for b in &f.body {
                stack.push(b);
            }
        }
        push_stmt_children(s, &mut stack);
        for e in stmt_exprs(s) {
            count_expr_lambdas(e, &mut count);
        }
    }
    count
}

fn count_expr_lambdas(e: &Expr, count: &mut u32) {
    // stmt_exprs already flattens the full expression tree (push_expr_tree
    // descends calls, bins, comps) — the lambda is present exactly once.
    // Any descent here would double-count.
    if matches!(e, Expr::Lambda(_)) {
        *count += 1;
    }
}

/// A module with exactly one top-level class whose name doesn't match the
/// file stem — the class-module rule.
pub fn class_module_findings(state: &mut ScanState, module_body: &[Stmt], rel: &str) {
    if rel.ends_with("__init__.py") {
        return;
    }
    let classes: Vec<&Stmt> = module_body.iter().filter(|s| matches!(s, Stmt::ClassDef(_))).collect();
    if classes.len() != 1 {
        return;
    }
    let Stmt::ClassDef(cls) = classes[0] else { return };
    let stem = rel
        .rsplit('/')
        .next()
        .unwrap_or(rel)
        .trim_end_matches(".py")
        .to_lowercase();
    let name = cls.name.to_lowercase();
    if name == stem || name == stem.replace('_', "") {
        return;
    }
    // Python's ast.ClassDef.lineno is the `class` keyword line; ruff's range
    // starts at the first decorator — use the name's range for parity
    let line = line_of(state.source, cls.name.range().start());
    state.findings.push(Finding {
        file: state.file.to_string(),
        line,
        function: cls.name.to_string(),
        kind: "class-module".into(),
        severity: "fail".into(),
        message: format!(
            "module '{rel}' holds one class '{}' — rename the file to {}.py (exception: closely related models)",
            cls.name.as_str(),
            cls.name.to_lowercase()
        ),
    });
}

/// Vague role-suffix class names hiding load-bearing code.
pub fn vague_name_findings(state: &mut ScanState, module_body: &[Stmt]) {
    for s in module_body {
        let Stmt::ClassDef(cls) = s else { continue };
        for suffix in VAGUE_SUFFIXES {
            if !cls.name.as_str().ends_with(suffix) {
                continue;
            }
            let methods = cls.body.iter().filter(|m| matches!(m, Stmt::FunctionDef(_))).count();
            let span =
                line_of(state.source, cls.range().end()).saturating_sub(line_of(state.source, cls.range().start()));
            if span < 120 && methods < 6 {
                break; // thin role class — the name is the communication
            }
            let line = stmt_line(state.source, s);
            state.findings.push(Finding {
                file: state.file.to_string(),
                line,
                function: cls.name.to_string(),
                kind: "vague-name".into(),
                severity: "fail".into(),
                message: format!(
                    "'{suffix}' name carries a {span}-line class with {methods} methods — the domain noun should take the name"
                ),
            });
            break;
        }
    }
}

/// Strewing: 3+ free functions sharing a leading parameter that is a class
/// defined in this module — a missed class.
pub fn strewing_findings(state: &mut ScanState, module_body: &[Stmt]) {
    let class_names: Vec<String> = module_body
        .iter()
        .filter_map(|s| match s {
            Stmt::ClassDef(c) => Some(c.name.to_string()),
            _ => None,
        })
        .collect();
    let mut groups: std::collections::HashMap<String, Vec<(String, usize)>> = std::collections::HashMap::new();
    for s in module_body {
        let Stmt::FunctionDef(f) = s else { continue };
        if f.parameters.args.is_empty() {
            continue;
        }
        let ann = match &f.parameters.args[0].parameter.annotation {
            Some(a) => annotation_base_name(a),
            None => None,
        };
        if let Some(base) = ann {
            if class_names.contains(&base) {
                groups
                    .entry(base)
                    .or_default()
                    .push((f.name.to_string(), stmt_line(state.source, s)));
            }
        }
    }
    for (base, mut members) in groups {
        if members.len() < 3 {
            continue;
        }
        members.sort_by_key(|m| m.1);
        let names: Vec<String> = members.iter().map(|(n, l)| format!("{n} (line {l})")).collect();
        let line = members[0].1;
        state.findings.push(Finding {
            file: state.file.to_string(),
            line,
            function: String::new(),
            kind: "strewing".into(),
            severity: "fail".into(),
            message: format!(
                "{} free functions share leading parameter '{base}' — a {base} class is missing (function strewing is a missed class): {}",
                members.len(),
                names.join(", ")
            ),
        });
    }
}

/// Names the function returns at its own top level (nested functions excluded).
pub fn returned_names(body: &[Stmt]) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut stack: Vec<&Stmt> = body.iter().collect();
    while let Some(s) = stack.pop() {
        if let Stmt::FunctionDef(_) = s {
            continue; // nested function bodies excluded
        }
        if let Stmt::Return(r) = s {
            if let Some(v) = &r.value {
                if let Expr::Name(n) = v.as_ref() {
                    out.insert(n.id.to_string());
                }
            }
        }
        push_stmt_children(s, &mut stack);
    }
    out
}

/// Module-scope containers mutated inside functions are still module state
/// (`_oauth_states: dict = {}` populated by login) — mirrors the Python
/// mutation handlers. Mutations of containers whose module literal is
/// ALREADY flagged are skipped (Python's `flagged` set — no double report).
pub fn mutation_findings(state: &mut ScanState, stmt: &Stmt) {
    if state.fn_stack.is_empty() || state.module_mutables.is_empty() {
        return;
    }
    let targets: Vec<&Expr> = match stmt {
        Stmt::Assign(a) => a.targets.iter().collect(),
        Stmt::AugAssign(a) => vec![&a.target],
        Stmt::AnnAssign(a) => vec![a.target.as_ref()],
        Stmt::Delete(d) => d.targets.iter().collect(),
        _ => return,
    };
    for target in targets {
        let container: Option<String> = match target {
            Expr::Name(n) if state.module_mutables.contains(n.id.as_str()) => Some(n.id.to_string()),
            Expr::Subscript(s) if state.module_mutables.contains(sub_name(s).as_str()) => Some(sub_name(s)),
            _ => None,
        };
        if let Some(container) = container {
            if state.module_flagged.contains(&container) {
                continue; // the module literal itself is already a finding
            }
            state.findings.push(Finding {
                file: state.file.to_string(),
                line: stmt_line(state.source, stmt),
                function: String::new(),
                kind: "global-state".into(),
                severity: "fail".into(),
                message: format!(
                    "module-level collection '{container}' is mutated inside a function — no module-level mutable state"
                ),
            });
        }
    }
}

fn sub_name(s: &ruff_python_ast::ExprSubscript) -> String {
    match s.value.as_ref() {
        Expr::Name(n) => n.id.to_string(),
        _ => String::new(),
    }
}

/// Names assigned List/Dict/Set literals at module scope — containers whose
/// in-function mutation is module state.
pub fn module_container_names(body: &[Stmt]) -> HashSet<String> {
    let mut out = HashSet::new();
    for s in body {
        let value = match s {
            Stmt::Assign(a) => Some(&a.value),
            Stmt::AnnAssign(a) => a.value.as_ref(),
            _ => None,
        };
        if let Some(v) = value {
            if matches!(v.as_ref(), Expr::List(_) | Expr::Dict(_) | Expr::Set(_)) {
                match s {
                    Stmt::Assign(a) => {
                        for t in &a.targets {
                            if let Expr::Name(n) = t {
                                out.insert(n.id.to_string());
                            }
                        }
                    }
                    Stmt::AnnAssign(a) => {
                        if let Expr::Name(n) = a.target.as_ref() {
                            out.insert(n.id.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

/// Module containers whose module-level literal is non-constant — already
/// flagged, so their mutations are not reported again (Python's `flagged`).
pub fn module_flagged_names(body: &[Stmt]) -> HashSet<String> {
    let mut out = HashSet::new();
    for s in body {
        let value = match s {
            Stmt::Assign(a) => Some(a.value.as_ref()),
            Stmt::AnnAssign(a) => a.value.as_deref(),
            _ => None,
        };
        if let Some(v) = value {
            if matches!(v, Expr::List(_) | Expr::Dict(_) | Expr::Set(_)) && !all_constant(v) {
                match s {
                    Stmt::Assign(a) => {
                        for t in &a.targets {
                            if let Expr::Name(n) = t {
                                out.insert(n.id.to_string());
                            }
                        }
                    }
                    Stmt::AnnAssign(a) => {
                        if let Expr::Name(n) = a.target.as_ref() {
                            out.insert(n.id.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

// =====================================================================
// repo-wide families: duplicate (Dice on structural skeletons) + unused
// (defined-but-never-referenced). These are computed in the Rust runner
// across ALL files of one invocation — mirroring _duplicate_actions and
// _unused_actions in code_health.py.
// =====================================================================

pub use crate::common::{dice_similarity, SkeletonFn};

/// The structural fingerprint: node types in CPython `ast.walk` BFS order,
/// with names/constants/args collapsed (`_fn_skeleton`). The BFS child
/// enumeration replicates `ast.iter_child_nodes` field order — the
/// Parameters node emits its defaults hoisted (kw_defaults then defaults),
/// dict literals emit all keys before all values, and if/elif chains
/// re-expand into nested "If" nodes, exactly like CPython's orelse nesting.
/// A BFS queue item — either an AST node or a bare operator/context token
/// (CPython visits `ast.operator` / `ast.expr_context` nodes in `ast.walk`).
pub enum Q<'a> {
    N(AnyNodeRef<'a>),
    T(&'static str),
}

pub fn fn_skeleton(f: &StmtFunctionDef) -> Vec<String> {
    use ruff_python_ast::AnyNodeRef;
    let mut toks: Vec<String> = Vec::new();
    let mut queue: Vec<Q> = Vec::new();
    toks.push(if f.is_async { "AsyncFunctionDef" } else { "FunctionDef" }.to_string());
    // CPython FunctionDef children: arguments, body, decorator_list, returns
    queue.push(Q::N(AnyNodeRef::Parameters(&f.parameters)));
    for s in &f.body {
        queue.push(Q::N(AnyNodeRef::from(s)));
    }
    for d in &f.decorator_list {
        queue.push(Q::N(AnyNodeRef::from(&d.expression)));
    }
    if let Some(r) = &f.returns {
        queue.push(Q::N(AnyNodeRef::from(r.as_ref())));
    }
    let mut i = 0usize;
    while i < queue.len() {
        let node = match &queue[i] {
            Q::N(n) => *n,
            Q::T(t) => {
                toks.push((*t).to_string());
                i += 1;
                continue;
            }
        };
        i += 1;
        match node {
            AnyNodeRef::ExprName(_) => toks.push("N".to_string()),
            AnyNodeRef::ExprStringLiteral(_)
            | AnyNodeRef::ExprBytesLiteral(_)
            | AnyNodeRef::ExprNumberLiteral(_)
            | AnyNodeRef::ExprBooleanLiteral(_)
            | AnyNodeRef::ExprNoneLiteral(_)
            | AnyNodeRef::ExprEllipsisLiteral(_)
            | AnyNodeRef::InterpolatedStringLiteralElement(_) => toks.push("C".to_string()),
            AnyNodeRef::Parameter(_) | AnyNodeRef::ParameterWithDefault(_) => toks.push("A".to_string()),
            AnyNodeRef::Parameters(_) => toks.push("arguments".to_string()),
            AnyNodeRef::ElifElseClause(_) => toks.push("If".to_string()),
            AnyNodeRef::InterpolatedElement(_) => toks.push("FormattedValue".to_string()),
            _ => toks.push(format!("{:?}", node.kind())),
        }
        skel_children(node, &mut queue);
    }
    toks
}

fn op_token(op: &Operator) -> &'static str {
    use ruff_python_ast::Operator as O;
    match op {
        O::Add => "Add",
        O::Sub => "Sub",
        O::Mult => "Mult",
        O::MatMult => "MatMult",
        O::Div => "Div",
        O::Mod => "Mod",
        O::Pow => "Pow",
        O::LShift => "LShift",
        O::RShift => "RShift",
        O::BitOr => "BitOr",
        O::BitXor => "BitXor",
        O::BitAnd => "BitAnd",
        O::FloorDiv => "FloorDiv",
    }
}

fn unary_op_token(op: &UnaryOp) -> &'static str {
    use ruff_python_ast::UnaryOp as U;
    match op {
        U::Invert => "Invert",
        U::Not => "Not",
        U::UAdd => "UAdd",
        U::USub => "USub",
    }
}

fn bool_op_token(op: &BoolOp) -> &'static str {
    use ruff_python_ast::BoolOp as B;
    match op {
        B::And => "And",
        B::Or => "Or",
    }
}

fn cmp_op_token(op: &CmpOp) -> &'static str {
    use ruff_python_ast::CmpOp as C;
    match op {
        C::Eq => "Eq",
        C::NotEq => "NotEq",
        C::Lt => "Lt",
        C::LtE => "LtE",
        C::Gt => "Gt",
        C::GtE => "GtE",
        C::Is => "Is",
        C::IsNot => "IsNot",
        C::In => "In",
        C::NotIn => "NotIn",
    }
}

fn ctx_token(ctx: &ExprContext) -> &'static str {
    use ruff_python_ast::ExprContext as X;
    match ctx {
        X::Load => "Load",
        X::Store => "Store",
        X::Del => "Del",
        X::Invalid => "Invalid",
    }
}

// reason: the match arms mirror CPython's ast.iter_child_nodes field order —
// one exhaustive table, splitting it would scatter a single mapping
#[allow(clippy::too_many_lines)]
/// Push a pattern node onto the skeleton queue.
fn push_pattern<'a>(queue: &mut Vec<Q<'a>>, p: &'a Pattern) {
    let n = match p {
        Pattern::MatchValue(x) => AnyNodeRef::PatternMatchValue(x),
        Pattern::MatchSingleton(x) => AnyNodeRef::PatternMatchSingleton(x),
        Pattern::MatchSequence(x) => AnyNodeRef::PatternMatchSequence(x),
        Pattern::MatchMapping(x) => AnyNodeRef::PatternMatchMapping(x),
        Pattern::MatchClass(x) => AnyNodeRef::PatternMatchClass(x),
        Pattern::MatchStar(x) => AnyNodeRef::PatternMatchStar(x),
        Pattern::MatchAs(x) => AnyNodeRef::PatternMatchAs(x),
        Pattern::MatchOr(x) => AnyNodeRef::PatternMatchOr(x),
    };
    queue.push(Q::N(n));
}

/// One BFS queue item — the walker's two push helpers (module-level: a
/// nested pair would hide structure the latent-class rule exists to surface).
fn skel_push<'a>(q: &mut Vec<Q<'a>>, n: AnyNodeRef<'a>) {
    q.push(Q::N(n));
}

fn skel_tok<'a>(q: &mut Vec<Q<'a>>, t: &'static str) {
    q.push(Q::T(t));
}

// code-health: ignore large-function the parity-locked walker mirrors CPython's field order — one dispatch table
pub fn skel_children<'a>(node: AnyNodeRef<'a>, queue: &mut Vec<Q<'a>>) {
    use ruff_python_ast::Pattern;
    match node {
        AnyNodeRef::StmtClassDef(c) => {
            // CPython: bases, keywords, body, decorator_list
            if let Some(arguments) = &c.arguments {
                for b in &arguments.args {
                    skel_push(queue, AnyNodeRef::from(b));
                }
                for k in &arguments.keywords {
                    skel_push(queue, AnyNodeRef::Keyword(k));
                }
            }
            for s in &c.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
            for d in &c.decorator_list {
                skel_push(queue, AnyNodeRef::from(&d.expression));
            }
        }
        AnyNodeRef::StmtReturn(r) => {
            if let Some(v) = &r.value {
                skel_push(queue, AnyNodeRef::from(v.as_ref()));
            }
        }
        AnyNodeRef::StmtDelete(d) => {
            for t in &d.targets {
                skel_push(queue, AnyNodeRef::from(t));
            }
        }
        AnyNodeRef::StmtTypeAlias(t) => {
            skel_push(queue, AnyNodeRef::from(t.name.as_ref()));
            if let Some(tp) = &t.type_params {
                for p in &tp.type_params {
                    match p {
                        ruff_python_ast::TypeParam::TypeVar(t) => skel_push(queue, AnyNodeRef::TypeParamTypeVar(t)),
                        ruff_python_ast::TypeParam::TypeVarTuple(t) => skel_push(queue, AnyNodeRef::TypeParamTypeVarTuple(t)),
                        ruff_python_ast::TypeParam::ParamSpec(t) => skel_push(queue, AnyNodeRef::TypeParamParamSpec(t)),
                    }
                }
            }
            skel_push(queue, AnyNodeRef::from(t.value.as_ref()));
        }
        AnyNodeRef::StmtAssign(a) => {
            for t in &a.targets {
                skel_push(queue, AnyNodeRef::from(t));
            }
            skel_push(queue, AnyNodeRef::from(a.value.as_ref()));
        }
        AnyNodeRef::StmtAugAssign(a) => {
            skel_push(queue, AnyNodeRef::from(a.target.as_ref()));
            skel_tok(queue, op_token(&a.op));
            skel_push(queue, AnyNodeRef::from(a.value.as_ref()));
        }
        AnyNodeRef::StmtAnnAssign(a) => {
            skel_push(queue, AnyNodeRef::from(a.target.as_ref()));
            skel_push(queue, AnyNodeRef::from(a.annotation.as_ref()));
            if let Some(v) = &a.value {
                skel_push(queue, AnyNodeRef::from(v.as_ref()));
            }
        }
        AnyNodeRef::StmtFor(f) => {
            skel_push(queue, AnyNodeRef::from(f.target.as_ref()));
            skel_push(queue, AnyNodeRef::from(f.iter.as_ref()));
            for s in &f.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
            for s in &f.orelse {
                skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtWhile(w) => {
            skel_push(queue, AnyNodeRef::from(w.test.as_ref()));
            for s in &w.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
            for s in &w.orelse {
                skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtIf(i) => {
            skel_push(queue, AnyNodeRef::from(i.test.as_ref()));
            for s in &i.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
            // CPython re-nests elifs as inner If nodes in orelse
            for clause in &i.elif_else_clauses {
                skel_push(queue, AnyNodeRef::ElifElseClause(clause));
            }
        }
        AnyNodeRef::ElifElseClause(clause) => {
            if let Some(t) = &clause.test {
                skel_push(queue, AnyNodeRef::from(t));
            }
            for s in &clause.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtWith(w) => {
            for item in &w.items {
                skel_push(queue, AnyNodeRef::WithItem(item));
            }
            for s in &w.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::WithItem(item) => {
            skel_push(queue, AnyNodeRef::from(&item.context_expr));
            if let Some(v) = &item.optional_vars {
                skel_push(queue, AnyNodeRef::from(v.as_ref()));
            }
        }
        AnyNodeRef::StmtMatch(m) => {
            skel_push(queue, AnyNodeRef::from(m.subject.as_ref()));
            for case in &m.cases {
                skel_push(queue, AnyNodeRef::MatchCase(case));
            }
        }
        AnyNodeRef::MatchCase(case) => {
            match &case.pattern {
                Pattern::MatchValue(p) => skel_push(queue, AnyNodeRef::PatternMatchValue(p)),
                Pattern::MatchSingleton(p) => skel_push(queue, AnyNodeRef::PatternMatchSingleton(p)),
                Pattern::MatchSequence(p) => skel_push(queue, AnyNodeRef::PatternMatchSequence(p)),
                Pattern::MatchMapping(p) => skel_push(queue, AnyNodeRef::PatternMatchMapping(p)),
                Pattern::MatchClass(p) => skel_push(queue, AnyNodeRef::PatternMatchClass(p)),
                Pattern::MatchStar(p) => skel_push(queue, AnyNodeRef::PatternMatchStar(p)),
                Pattern::MatchAs(p) => skel_push(queue, AnyNodeRef::PatternMatchAs(p)),
                Pattern::MatchOr(p) => skel_push(queue, AnyNodeRef::PatternMatchOr(p)),
            }
            if let Some(g) = &case.guard {
                skel_push(queue, AnyNodeRef::from(g.as_ref()));
            }
            for s in &case.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtRaise(r) => {
            if let Some(e) = &r.exc {
                skel_push(queue, AnyNodeRef::from(e.as_ref()));
            }
            if let Some(c) = &r.cause {
                skel_push(queue, AnyNodeRef::from(c.as_ref()));
            }
        }
        AnyNodeRef::StmtTry(t) => {
            for s in &t.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
            for h in &t.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                skel_push(queue, AnyNodeRef::ExceptHandlerExceptHandler(eh));
            }
            for s in &t.orelse {
                skel_push(queue, AnyNodeRef::from(s));
            }
            for s in &t.finalbody {
                skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::ExceptHandlerExceptHandler(eh) => {
            if let Some(t) = &eh.type_ {
                skel_push(queue, AnyNodeRef::from(t.as_ref()));
            }
            for s in &eh.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtAssert(a) => {
            skel_push(queue, AnyNodeRef::from(a.test.as_ref()));
            if let Some(m) = &a.msg {
                skel_push(queue, AnyNodeRef::from(m.as_ref()));
            }
        }
        AnyNodeRef::StmtImport(imp) => {
            for a in &imp.names {
                skel_push(queue, AnyNodeRef::Alias(a));
            }
        }
        AnyNodeRef::StmtImportFrom(imp) => {
            for a in &imp.names {
                skel_push(queue, AnyNodeRef::Alias(a));
            }
        }
        AnyNodeRef::StmtExpr(e) => skel_push(queue, AnyNodeRef::from(e.value.as_ref())),
        // a nested function's subtree IS part of ast.walk — descend like the
        // module-level root: arguments, body, decorator_list, returns
        AnyNodeRef::StmtFunctionDef(f) => {
            skel_push(queue, AnyNodeRef::Parameters(&f.parameters));
            for s in &f.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
            for d in &f.decorator_list {
                skel_push(queue, AnyNodeRef::from(&d.expression));
            }
            if let Some(r) = &f.returns {
                skel_push(queue, AnyNodeRef::from(r.as_ref()));
            }
        }
        // leaf / identifier-only statements
        AnyNodeRef::StmtGlobal(_)
        | AnyNodeRef::StmtNonlocal(_)
        | AnyNodeRef::StmtPass(_)
        | AnyNodeRef::StmtBreak(_)
        | AnyNodeRef::StmtContinue(_)
        | AnyNodeRef::StmtIpyEscapeCommand(_)
        | AnyNodeRef::Alias(_) => {}
        AnyNodeRef::Parameters(p) => {
            // CPython arguments children: posonlyargs, args, vararg,
            // kwonlyargs, kw_defaults, kwarg, defaults (hoisted)
            for pwd in &p.posonlyargs {
                skel_push(queue, AnyNodeRef::ParameterWithDefault(pwd));
            }
            for pwd in &p.args {
                skel_push(queue, AnyNodeRef::ParameterWithDefault(pwd));
            }
            if let Some(v) = &p.vararg {
                skel_push(queue, AnyNodeRef::Parameter(v));
            }
            for pwd in &p.kwonlyargs {
                skel_push(queue, AnyNodeRef::ParameterWithDefault(pwd));
            }
            for pwd in p.kwonlyargs.iter().filter_map(|p| p.default.as_deref()) {
                skel_push(queue, AnyNodeRef::from(pwd));
            }
            if let Some(k) = &p.kwarg {
                skel_push(queue, AnyNodeRef::Parameter(k));
            }
            for pwd in p
                .posonlyargs
                .iter()
                .chain(&p.args)
                .filter_map(|p| p.default.as_deref())
            {
                skel_push(queue, AnyNodeRef::from(pwd));
            }
        }
        AnyNodeRef::Parameter(param) => {
            if let Some(a) = &param.annotation {
                skel_push(queue, AnyNodeRef::from(a.as_ref()));
            }
        }
        AnyNodeRef::ParameterWithDefault(pwd) => {
            // the default is hoisted by Parameters (CPython defaults list)
            if let Some(a) = &pwd.parameter.annotation {
                skel_push(queue, AnyNodeRef::from(a.as_ref()));
            }
        }
        AnyNodeRef::Keyword(k) => skel_push(queue, AnyNodeRef::from(&k.value)),
        AnyNodeRef::Comprehension(c) => {
            skel_push(queue, AnyNodeRef::from(&c.target));
            skel_push(queue, AnyNodeRef::from(&c.iter));
            for f in &c.ifs {
                skel_push(queue, AnyNodeRef::from(f));
            }
        }
        AnyNodeRef::ExprBoolOp(b) => {
            skel_tok(queue, bool_op_token(&b.op));
            for v in &b.values {
                skel_push(queue, AnyNodeRef::from(v));
            }
        }
        AnyNodeRef::ExprNamed(n) => {
            skel_push(queue, AnyNodeRef::from(n.target.as_ref()));
            skel_push(queue, AnyNodeRef::from(n.value.as_ref()));
        }
        AnyNodeRef::ExprBinOp(b) => {
            skel_push(queue, AnyNodeRef::from(b.left.as_ref()));
            skel_tok(queue, op_token(&b.op));
            skel_push(queue, AnyNodeRef::from(b.right.as_ref()));
        }
        AnyNodeRef::ExprUnaryOp(u) => {
            skel_tok(queue, unary_op_token(&u.op));
            skel_push(queue, AnyNodeRef::from(u.operand.as_ref()));
        }
        AnyNodeRef::ExprLambda(l) => {
            if let Some(p) = &l.parameters {
                skel_push(queue, AnyNodeRef::Parameters(p));
            }
            skel_push(queue, AnyNodeRef::from(l.body.as_ref()));
        }
        AnyNodeRef::ExprIf(e) => {
            skel_push(queue, AnyNodeRef::from(e.test.as_ref()));
            skel_push(queue, AnyNodeRef::from(e.body.as_ref()));
            skel_push(queue, AnyNodeRef::from(e.orelse.as_ref()));
        }
        AnyNodeRef::ExprDict(d) => {
            // CPython: all keys first, then all values
            for item in &d.items {
                if let Some(k) = &item.key {
                    skel_push(queue, AnyNodeRef::from(k));
                }
            }
            for item in &d.items {
                skel_push(queue, AnyNodeRef::from(&item.value));
            }
        }
        AnyNodeRef::ExprSet(s) => {
            for e in &s.elts {
                skel_push(queue, AnyNodeRef::from(e));
            }
        }
        AnyNodeRef::ExprListComp(c) => {
            skel_push(queue, AnyNodeRef::from(c.elt.as_ref()));
            for g in &c.generators {
                skel_push(queue, AnyNodeRef::Comprehension(g));
            }
        }
        AnyNodeRef::ExprSetComp(c) => {
            skel_push(queue, AnyNodeRef::from(c.elt.as_ref()));
            for g in &c.generators {
                skel_push(queue, AnyNodeRef::Comprehension(g));
            }
        }
        AnyNodeRef::ExprGenerator(g) => {
            skel_push(queue, AnyNodeRef::from(g.elt.as_ref()));
            for gen in &g.generators {
                skel_push(queue, AnyNodeRef::Comprehension(gen));
            }
        }
        AnyNodeRef::ExprDictComp(c) => {
            if let Some(k) = &c.key {
                skel_push(queue, AnyNodeRef::from(k.as_ref()));
            }
            skel_push(queue, AnyNodeRef::from(c.value.as_ref()));
            for g in &c.generators {
                skel_push(queue, AnyNodeRef::Comprehension(g));
            }
        }
        AnyNodeRef::ExprAwait(a) => skel_push(queue, AnyNodeRef::from(a.value.as_ref())),
        AnyNodeRef::ExprYield(y) => {
            if let Some(v) = &y.value {
                skel_push(queue, AnyNodeRef::from(v.as_ref()));
            }
        }
        AnyNodeRef::ExprYieldFrom(y) => skel_push(queue, AnyNodeRef::from(y.value.as_ref())),
        AnyNodeRef::ExprCompare(c) => {
            skel_push(queue, AnyNodeRef::from(c.left.as_ref()));
            for o in &c.ops {
                skel_tok(queue, cmp_op_token(o));
            }
            for o in &c.comparators {
                skel_push(queue, AnyNodeRef::from(o));
            }
        }
        AnyNodeRef::ExprCall(c) => {
            skel_push(queue, AnyNodeRef::from(c.func.as_ref()));
            for a in &c.arguments.args {
                skel_push(queue, AnyNodeRef::from(a));
            }
            for k in &c.arguments.keywords {
                skel_push(queue, AnyNodeRef::Keyword(k));
            }
        }
        AnyNodeRef::ExprFString(f) => {
            for element in f.value.elements() {
                skel_push(queue, AnyNodeRef::from(element));
            }
        }
        AnyNodeRef::ExprTString(t) => {
            // t-strings (3.14) — structurally a JoinedStr; walk parts' elements
            for element in t.value.elements() {
                skel_push(queue, AnyNodeRef::from(element));
            }
        }
        AnyNodeRef::InterpolatedElement(e) => {
            skel_push(queue, AnyNodeRef::from(e.expression.as_ref()));
            if let Some(spec) = &e.format_spec {
                for element in &spec.elements {
                    skel_push(queue, AnyNodeRef::from(element));
                }
            }
        }
        AnyNodeRef::InterpolatedStringFormatSpec(_) | AnyNodeRef::InterpolatedStringLiteralElement(_) => {}
        AnyNodeRef::ExprAttribute(a) => {
            skel_push(queue, AnyNodeRef::from(a.value.as_ref()));
            skel_tok(queue, ctx_token(&a.ctx));
        }
        AnyNodeRef::ExprSubscript(s) => {
            skel_push(queue, AnyNodeRef::from(s.value.as_ref()));
            skel_push(queue, AnyNodeRef::from(s.slice.as_ref()));
            skel_tok(queue, ctx_token(&s.ctx));
        }
        AnyNodeRef::ExprStarred(s) => {
            skel_push(queue, AnyNodeRef::from(s.value.as_ref()));
            skel_tok(queue, ctx_token(&s.ctx));
        }
        AnyNodeRef::ExprList(l) => {
            for e in &l.elts {
                skel_push(queue, AnyNodeRef::from(e));
            }
            skel_tok(queue, ctx_token(&l.ctx));
        }
        AnyNodeRef::ExprTuple(t) => {
            for e in &t.elts {
                skel_push(queue, AnyNodeRef::from(e));
            }
            skel_tok(queue, ctx_token(&t.ctx));
        }
        AnyNodeRef::ExprSlice(s) => {
            if let Some(l) = &s.lower {
                skel_push(queue, AnyNodeRef::from(l.as_ref()));
            }
            if let Some(u) = &s.upper {
                skel_push(queue, AnyNodeRef::from(u.as_ref()));
            }
            if let Some(st) = &s.step {
                skel_push(queue, AnyNodeRef::from(st.as_ref()));
            }
        }
        AnyNodeRef::PatternMatchValue(p) => skel_push(queue, AnyNodeRef::from(p.value.as_ref())),
        AnyNodeRef::PatternMatchSequence(p) => {
            for pat in &p.patterns {
                push_pattern(queue, pat);
            }
        }
        AnyNodeRef::PatternMatchMapping(p) => {
            for k in &p.keys {
                skel_push(queue, AnyNodeRef::from(k));
            }
            for pat in &p.patterns {
                push_pattern(queue, pat);
            }
        }
        AnyNodeRef::PatternMatchClass(p) => {
            skel_push(queue, AnyNodeRef::from(p.cls.as_ref()));
            for pat in &p.arguments.patterns {
                push_pattern(queue, pat);
            }
            for kw in &p.arguments.keywords {
                push_pattern(queue, &kw.pattern);
            }
        }
        AnyNodeRef::PatternMatchAs(p) => {
            if let Some(pat) = &p.pattern {
                push_pattern(queue, pat);
            }
        }
        AnyNodeRef::PatternMatchOr(p) => {
            for pat in &p.patterns {
                push_pattern(queue, pat);
            }
        }
        AnyNodeRef::PatternMatchSingleton(_) | AnyNodeRef::PatternMatchStar(_) => {}
        AnyNodeRef::TypeParamTypeVar(t) => {
            if let Some(b) = &t.bound {
                skel_push(queue, AnyNodeRef::from(b.as_ref()));
            }
            if let Some(d) = &t.default {
                skel_push(queue, AnyNodeRef::from(d.as_ref()));
            }
        }
        AnyNodeRef::TypeParamTypeVarTuple(t) => {
            if let Some(d) = &t.default {
                skel_push(queue, AnyNodeRef::from(d.as_ref()));
            }
        }
        AnyNodeRef::TypeParamParamSpec(t) => {
            if let Some(d) = &t.default {
                skel_push(queue, AnyNodeRef::from(d.as_ref()));
            }
        }
        AnyNodeRef::PatternArguments(_) | AnyNodeRef::PatternKeyword(_) => {}
        AnyNodeRef::Arguments(a) => {
            for e in &a.args {
                skel_push(queue, AnyNodeRef::from(e));
            }
            for k in &a.keywords {
                skel_push(queue, AnyNodeRef::Keyword(k));
            }
        }
        // literals, names, module — leaves
        AnyNodeRef::ExprName(n) => skel_tok(queue, ctx_token(&n.ctx)),
        AnyNodeRef::ExprStringLiteral(_)
        | AnyNodeRef::ExprBytesLiteral(_)
        | AnyNodeRef::ExprNumberLiteral(_)
        | AnyNodeRef::ExprBooleanLiteral(_)
        | AnyNodeRef::ExprNoneLiteral(_)
        | AnyNodeRef::ExprEllipsisLiteral(_)
        | AnyNodeRef::ExprIpyEscapeCommand(_)
        | AnyNodeRef::ModModule(_)
        | AnyNodeRef::ModExpression(_)
        // never pushed by skel_children, but the match must be total
        | AnyNodeRef::Decorator(_)
        | AnyNodeRef::TypeParams(_)
        | AnyNodeRef::FString(_)
        | AnyNodeRef::TString(_)
        | AnyNodeRef::StringLiteral(_)
        | AnyNodeRef::BytesLiteral(_)
        | AnyNodeRef::Identifier(_) => {}
    }
}

/// `_is_duplicate_candidate`: at least two real statements (one-line
/// accessors are not copy-paste) and a 12+ token skeleton; `__init__`
/// boilerplate is conventional.
pub fn is_duplicate_candidate(f: &StmtFunctionDef, skeleton_len: usize) -> bool {
    if f.name.as_str() == "__init__" {
        return false;
    }
    let stmts: Vec<&Stmt> = f
        .body
        .iter()
        .filter(|s| !matches!(s, Stmt::Expr(e) if matches!(e.value.as_ref(), Expr::StringLiteral(_))))
        .collect();
    stmts.len() >= 2 && skeleton_len >= 12
}

pub use crate::common::bigram_set_hash;

/// Cross-file copy-paste findings (`_duplicate_actions`).
///
/// Not O(n²): the tolerance guard (`|len_a - len_b| <= max(2, len/5)`) means
/// only skeletons of similar length can ever match, so candidates are
/// bucketed by skeleton length and each one probes only the lengths within
/// its window — near-linear for real repos, and the all-same-length
/// degenerate case is genuinely pairwise (every candidate is a valid peer).
/// `_first_duplicate` semantics are preserved exactly: the earliest LATER
/// candidate (list order) at >= 90% Dice.
///
/// Exact matches need no similarity math: dice is 1.0 iff the bigram SETS
/// are equal (identical sets have equal cardinality), so a content-addressed
/// set hash — XOR of per-bigram hashes, order-independent — is an O(1)
/// collision test. Same-hash pairs skip the Dice computation entirely; the
/// length guard still applies (identical bigram sets can arise from periodic
/// sequences of different lengths, which the rule rejects).
pub fn duplicate_findings(fns: &[SkeletonFn]) -> Vec<Finding> {
    use std::collections::HashMap;
    let mut out = Vec::new();
    // precomputed set hashes — one O(len) pass per candidate, O(1) per pair
    let hashes: Vec<u64> = fns.iter().map(|fr| bigram_set_hash(&fr.skeleton)).collect();
    let mut buckets: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, fr) in fns.iter().enumerate() {
        buckets.entry(fr.skeleton.len()).or_default().push(i);
    }
    let mut len_keys: Vec<usize> = buckets.keys().copied().collect();
    len_keys.sort_unstable();
    for (i, fr) in fns.iter().enumerate() {
        let l = fr.skeleton.len();
        let tol = (l / 5).max(2);
        let lo = l.saturating_sub(tol);
        let hi = l + tol;
        let mut best: Option<(usize, f64)> = None;
        for &len in len_keys.iter().filter(|&&k| k >= lo && k <= hi) {
            let bucket = &buckets[&len];
            let start = bucket.partition_point(|&j| j <= i);
            for &j in &bucket[start..] {
                // identical bigram sets -> dice is exactly 1.0, no computation
                let sim = if hashes[j] == hashes[i] {
                    1.0
                } else {
                    dice_similarity(&fr.skeleton, &fns[j].skeleton)
                };
                if sim >= 0.9 {
                    if best.is_none_or(|(b, _)| j < b) {
                        best = Some((j, sim));
                    }
                    break; // later members of this bucket are later indices
                }
            }
        }
        if let Some((j, sim)) = best {
            let dup = &fns[j];
            out.push(Finding {
                file: dup.rel.clone(),
                line: dup.line,
                function: dup.name.clone(),
                kind: "duplicate".into(),
                severity: "warn".into(),
                message: format!(
                    "function '{}' ({}:{}) is {:.0}% similar to '{}' ({}:{}) — copy-paste; extract the shared logic into one function",
                    dup.name, dup.rel, dup.line, sim * 100.0, fr.name, fr.rel, fr.line
                ),
            });
        }
    }
    out
}

/// Dead-code findings (`_unused_actions`).
pub fn unused_findings(
    definitions: &[(String, String, usize)], // (rel, name, line)
    prod_refs: &HashSet<String>,
    test_refs: &HashSet<String>,
    strings: &[String],
) -> Vec<Finding> {
    let mut out = Vec::new();
    for (rel, name, line) in definitions {
        if name == "main" || prod_refs.contains(name) || strings.iter().any(|s| s.contains(name.as_str())) {
            continue;
        }
        let (message, kind) = if test_refs.contains(name) {
            (
                format!(
                    "function '{name}' ({rel}:{line}) is referenced only from tests — if it is a deliberate test seam (isolation hook, fixture helper), document it with `# code-health: ignore unused <why>`; otherwise production code that nothing ships calls is dead — delete it"
                ),
                "unused",
            )
        } else {
            (
                format!(
                    "function '{name}' ({rel}:{line}) is defined but never referenced — dead code is deleted, not kept (unless it is a CLI command or public API entry point)"
                ),
                "unused",
            )
        };
        out.push(Finding {
            file: rel.clone(),
            line: *line,
            function: name.clone(),
            kind: kind.into(),
            severity: "warn".into(),
            message,
        });
    }
    out
}

// =====================================================================
// record-shape: "never a bare dict as a record" (check_records.py)
//   - signatures: record-shaped collections in params/returns (grab-bags,
//     collections of dicts/tuples, nested lists, fixed tuples); maps pass;
//     deserializer boundaries (raw JSON in, domain class out) are exempt
//   - literals: dict literals with >= 2 keys, >= 1 constant string key,
//     >= 1 dynamic value, in a record position (assign/return/yield)
// =====================================================================

const PRIMITIVES: [&str; 8] = ["str", "int", "float", "bool", "bytes", "Any", "object", "None"];

/// `_name_of`: the bare name of an annotation (typing.Any -> Any).
fn ann_name_of(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => Some(a.attr.to_string()),
        Expr::NoneLiteral(_) => Some("None".to_string()),
        Expr::BooleanLiteral(b) => Some(b.value.to_string()),
        Expr::StringLiteral(s) => Some(s.value.to_str().to_string()),
        _ => None,
    }
}

/// `_base_name`: lowercase dict/list/tuple/optional spellings (typing.Dict -> dict).
fn ann_base_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.id.to_lowercase()),
        Expr::Attribute(a) => Some(a.attr.to_lowercase()),
        _ => None,
    }
}

/// `_unwrap`: peel Optional[..]/Union[..]/A | B wrappers into their members.
fn ann_unwrap<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    match e {
        Expr::BinOp(b) if matches!(b.op, Operator::BitOr) => {
            ann_unwrap(&b.left, out);
            ann_unwrap(&b.right, out);
        }
        Expr::Subscript(s) if matches!(ann_base_name(&s.value).as_deref(), Some("optional" | "union")) => {
            let slice: &Expr = s.slice.as_ref();
            if let Expr::Tuple(t) = slice {
                for elt in &t.elts {
                    ann_unwrap(elt, out);
                }
            } else {
                ann_unwrap(slice, out);
            }
        }
        _ => out.push(e),
    }
}

/// `_is_variadic_tuple`: tuple[T, ...] is a homogeneous sequence, not a pair.
fn is_variadic_tuple(e: &Expr) -> bool {
    if let Expr::Subscript(s) = e {
        if ann_base_name(&s.value).as_deref() == Some("tuple") {
            if let Expr::Tuple(t) = s.slice.as_ref() {
                return t.elts.iter().any(|elt| matches!(elt, Expr::EllipsisLiteral(_)));
            }
        }
    }
    false
}

/// `_is_raw_json`: bare/grab-bag dict (dict, dict[str, Any], ...) or a
/// collection of one — the deserializer-boundary exemption.
fn is_raw_json(e: &Expr) -> bool {
    let mut wrapped = Vec::new();
    ann_unwrap(e, &mut wrapped);
    if wrapped.len() != 1 {
        return wrapped.iter().any(|p| is_raw_json(p));
    }
    let node = wrapped[0];
    if let Expr::Name(n) = node {
        return n.id.eq_ignore_ascii_case("dict");
    }
    if let Expr::Subscript(s) = node {
        let base = ann_base_name(&s.value);
        if base.as_deref() == Some("dict") {
            let slice: &Expr = s.slice.as_ref();
            if let Expr::Tuple(t) = slice {
                if t.elts.len() != 2 {
                    return true; // malformed — treat as bare-ish
                }
                let mut val_parts = Vec::new();
                ann_unwrap(&t.elts[1], &mut val_parts);
                return val_parts
                    .iter()
                    .any(|p| matches!(ann_name_of(p).as_deref(), Some("Any" | "object" | "None")));
            }
            return true; // dict[X] single-arg
        }
        if matches!(base.as_deref(), Some("list" | "tuple")) {
            let elt: &Expr = s.slice.as_ref();
            if matches!(elt, Expr::Tuple(_)) {
                return false; // a fixed tuple of stuff is not raw rows
            }
            return is_raw_json(elt);
        }
    }
    false
}

/// `_annotation_is_record`: record-shaped bare collection in a signature.
fn annotation_is_record(e: &Expr) -> bool {
    let mut wrapped = Vec::new();
    ann_unwrap(e, &mut wrapped);
    if wrapped.len() != 1 {
        return wrapped.iter().any(|p| annotation_is_record(p));
    }
    let node = wrapped[0];
    if let Expr::Name(n) = node {
        return n.id.eq_ignore_ascii_case("dict"); // bare dict = grab-bag
    }
    if let Expr::Subscript(s) = node {
        let base = ann_base_name(&s.value);
        if base.as_deref() == Some("dict") {
            let slice: &Expr = s.slice.as_ref();
            if let Expr::Tuple(t) = slice {
                if t.elts.len() != 2 {
                    return false; // malformed — tolerate
                }
                let key = &t.elts[0];
                if !matches!(ann_name_of(key).as_deref(), Some("str" | "Any")) {
                    return false;
                }
                let mut val_parts = Vec::new();
                ann_unwrap(&t.elts[1], &mut val_parts);
                if val_parts.len() > 1 {
                    // a union value: a record or shapeless member makes it a record
                    return val_parts.iter().any(|p| annotation_is_record(p))
                        || val_parts
                            .iter()
                            .any(|p| matches!(ann_name_of(p).as_deref(), Some("Any" | "object")));
                }
                let val = val_parts[0];
                let val_name = ann_name_of(val);
                if matches!(val_name.as_deref(), Some("Any" | "object" | "None")) {
                    return true; // grab-bag: no shape
                }
                if matches!(val, Expr::Subscript(_)) {
                    return !is_variadic_tuple(val); // collection values are records
                }
                return matches!(ann_base_name(val).as_deref(), Some("dict" | "tuple" | "list"));
            }
            return true; // dict[X] single-arg or dict[()]
        }
        if base.as_deref() == Some("tuple") {
            return !is_variadic_tuple(node); // fixed-size pairs are records
        }
        if base.as_deref() == Some("list") {
            let mut parts = Vec::new();
            ann_unwrap(s.slice.as_ref(), &mut parts);
            if parts.len() != 1 {
                return parts.iter().any(|p| annotation_is_record(p));
            }
            let value = parts[0];
            if matches!(value, Expr::Subscript(_)) {
                return !is_variadic_tuple(value);
            }
            return matches!(
                value,
                Expr::Name(n) if matches!(n.id.to_lowercase().as_str(), "dict" | "tuple" | "list")
            );
        }
    }
    false
}

/// `_part_is_domain`: one return-annotation part resolving to a domain class.
fn part_is_domain(e: &Expr) -> bool {
    match e {
        Expr::Name(n) => !PRIMITIVES.contains(&n.id.as_str()) && !matches!(n.id.as_str(), "dict" | "tuple" | "list"),
        Expr::Subscript(s) => {
            if matches!(ann_base_name(&s.value).as_deref(), Some("list" | "tuple")) {
                let elt: &Expr = s.slice.as_ref();
                if let Expr::Tuple(t) = elt {
                    let parts: Vec<&Expr> = t
                        .elts
                        .iter()
                        .filter(|e| !matches!(e, Expr::EllipsisLiteral(_)))
                        .collect();
                    return parts.len() == 1 && part_is_domain(parts[0]);
                }
                return part_is_domain(elt);
            }
            false
        }
        _ => false,
    }
}

/// `_returns_domain_class`: the function converts raw JSON into domain
/// objects — the sanctioned deserializer boundary.
fn returns_domain_class(f: &StmtFunctionDef) -> bool {
    match &f.returns {
        Some(r) => {
            let mut parts = Vec::new();
            ann_unwrap(r.as_ref(), &mut parts);
            parts.iter().any(|p| part_is_domain(p))
        }
        None => false,
    }
}

/// `_is_constant_value`: a literal that cannot vary at runtime (lookup
/// tables may carry nested constant structures).
fn is_constant_value(e: &Expr) -> bool {
    match e {
        Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_) => true,
        Expr::UnaryOp(u) if matches!(u.op, UnaryOp::UAdd | UnaryOp::USub) => is_constant_value(&u.operand),
        Expr::List(l) => l.elts.iter().all(is_constant_value),
        Expr::Tuple(t) => t.elts.iter().all(is_constant_value),
        Expr::Dict(d) => d
            .items
            .iter()
            .all(|it| it.key.as_ref().map(is_constant_value).unwrap_or(false) && is_constant_value(&it.value)),
        _ => false,
    }
}

/// The dict-literal scan: record positions only; inline call arguments are
/// maps and are not descended into; spread merges are not records.
fn record_literal_scan(e: &Expr, source: &str, found: &mut Vec<usize>) {
    match e {
        Expr::Dict(d) => {
            // a spread merge ({**session, ...}) updates an existing shape —
            // not a record being built (ruff keys are None for the unpack)
            if d.items.iter().any(|it| it.key.is_none()) {
                return;
            }
            let has_const_key = d
                .items
                .iter()
                .any(|it| matches!(&it.key, Some(k) if matches!(k, Expr::StringLiteral(_))));
            let has_dynamic_value = d.items.iter().any(|it| !is_constant_value(&it.value));
            if d.items.len() >= 2 && has_const_key && has_dynamic_value {
                found.push(line_of(source, d.range().start()));
            }
            for it in &d.items {
                record_literal_scan(&it.value, source, found);
            }
        }
        Expr::List(l) => {
            for elt in &l.elts {
                record_literal_scan(elt, source, found);
            }
        }
        Expr::Tuple(t) => {
            for elt in &t.elts {
                record_literal_scan(elt, source, found);
            }
        }
        Expr::If(ie) => {
            record_literal_scan(&ie.body, source, found);
            record_literal_scan(&ie.orelse, source, found);
        }
        Expr::ListComp(c) => record_literal_scan(&c.elt, source, found),
        Expr::SetComp(c) => record_literal_scan(&c.elt, source, found),
        Expr::Generator(c) => record_literal_scan(&c.elt, source, found),
        Expr::DictComp(c) => {
            if let Some(k) = &c.key {
                record_literal_scan(k, source, found);
            }
            record_literal_scan(&c.value, source, found);
        }
        Expr::Lambda(l) => record_literal_scan(&l.body, source, found),
        _ => {} // NOT Call — inline arguments (headers={...}) are maps
    }
}

/// The record-shape family: signature findings + record dict literals,
/// walking the module in ast.walk BFS order (nested functions included).
pub fn record_shape_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            if let AnyNodeRef::StmtFunctionDef(f) = n {
                let def_line = line_of(source, f.name.range().start());
                let boundary = returns_domain_class(f);
                let mut params: Vec<(&str, Option<&Expr>)> = Vec::new();
                for pwd in f
                    .parameters
                    .posonlyargs
                    .iter()
                    .chain(&f.parameters.args)
                    .chain(&f.parameters.kwonlyargs)
                {
                    params.push((pwd.parameter.name.as_str(), pwd.parameter.annotation.as_deref()));
                }
                if let Some(v) = &f.parameters.vararg {
                    params.push((v.name.as_str(), v.annotation.as_deref()));
                }
                if let Some(k) = &f.parameters.kwarg {
                    params.push((k.name.as_str(), k.annotation.as_deref()));
                }
                for (arg, ann) in params {
                    if let Some(a) = ann {
                        if !annotation_is_record(a) {
                            continue;
                        }
                        if boundary && is_raw_json(a) {
                            continue; // deserializer boundary: raw JSON in, domain class out
                        }
                        let text = source[a.range()].to_string();
                        state.findings.push(Finding {
                            file: state.file.to_string(),
                            line: def_line,
                            function: f.name.to_string(),
                            kind: "record-shape".into(),
                            severity: "fail".into(),
                            message: format!(
                                "bare record collection '{text}' in parameter '{arg}' of {} (line {def_line})",
                                f.name.as_str()
                            ),
                        });
                    }
                }
                if let Some(r) = &f.returns {
                    if annotation_is_record(r.as_ref()) {
                        let text = source[r.range()].to_string();
                        state.findings.push(Finding {
                            file: state.file.to_string(),
                            line: def_line,
                            function: f.name.to_string(),
                            kind: "record-shape".into(),
                            severity: "fail".into(),
                            message: format!(
                                "bare record collection '{text}' as return type of {} (line {def_line})",
                                f.name.as_str()
                            ),
                        });
                    }
                }
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
    // record dict literals in record positions (assign/annassign/return/yield)
    let mut found: Vec<usize> = Vec::new();
    let mut sq: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut si = 0usize;
    while si < sq.len() {
        if let Q::N(n) = sq[si] {
            let value: Option<&Expr> = match n {
                AnyNodeRef::StmtAssign(a) => Some(a.value.as_ref()),
                AnyNodeRef::StmtAnnAssign(a) => a.value.as_deref(),
                AnyNodeRef::StmtReturn(r) => r.value.as_deref(),
                AnyNodeRef::ExprYield(y) => y.value.as_deref(),
                AnyNodeRef::ExprYieldFrom(y) => Some(y.value.as_ref()),
                _ => None,
            };
            if let Some(v) = value {
                record_literal_scan(v, source, &mut found);
            }
            skel_children(n, &mut sq);
        }
        si += 1;
    }
    for ln in found {
        state.findings.push(Finding {
            file: state.file.to_string(),
            line: ln,
            function: String::new(),
            kind: "record-shape".into(),
            severity: "fail".into(),
            message: format!("dict literal with constant keys is a record — make a class (line {ln})"),
        });
    }
}

// =====================================================================
// latent-class partition + the test-only rule families (monkeypatch,
// skipif, fakefs) — the last per-file Python families
// =====================================================================

/// `_partition_for_class`: a class whose methods split into >= 2
/// field-disjoint groups after removing at most 2 connectors.
fn partition_for_class(source: &str, cls: &StmtClassDef) -> Option<(Vec<String>, Vec<Vec<String>>)> {
    let methods: Vec<&Stmt> = cls.body.iter().filter(|s| matches!(s, Stmt::FunctionDef(_))).collect();
    if methods.len() < 6 {
        return None;
    }
    let span = line_of(source, cls.range().end()) - line_of(source, cls.name.range().start());
    if span < 150 {
        return None;
    }
    // method name -> set of self.<attr> fields accessed anywhere in the body
    let mut mf: Vec<(String, HashSet<String>)> = Vec::new();
    for m in &methods {
        let Stmt::FunctionDef(f) = m else { continue };
        let mut fields = HashSet::new();
        let mut queue: Vec<Q> = f.body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
        for d in &f.decorator_list {
            queue.push(Q::N(AnyNodeRef::from(&d.expression)));
        }
        let mut qi = 0usize;
        while qi < queue.len() {
            if let Q::N(n) = queue[qi] {
                if let AnyNodeRef::ExprAttribute(a) = n {
                    if let Expr::Name(name) = a.value.as_ref() {
                        if name.id == "self" {
                            fields.insert(a.attr.to_string());
                        }
                    }
                }
                skel_children(n, &mut queue);
            }
            qi += 1;
        }
        mf.push((f.name.to_string(), fields));
    }
    let names: Vec<String> = mf.iter().map(|(n, _)| n.clone()).collect();
    // smallest connector removal exposing >= 2 disjoint groups
    for removal in 0..3usize {
        for removed in combinations(&names, removal) {
            let kept: Vec<String> = names.iter().filter(|n| !removed.contains(n)).cloned().collect();
            let groups = connected_groups(&kept, &mf);
            let big: Vec<Vec<String>> = groups
                .into_iter()
                .filter(|g| {
                    let fields: HashSet<&String> = g
                        .iter()
                        .flat_map(|m| {
                            mf.iter()
                                .find(|(n, _)| n == m)
                                .map(|(_, f)| f.iter())
                                .unwrap_or_default()
                        })
                        .collect();
                    g.len() >= 2 && fields.len() >= 2
                })
                .collect();
            if big.len() >= 2 {
                return Some((removed, big));
            }
        }
    }
    None
}

/// combinations of `names` taken `k` at a time (k <= 2, so O(n^2) max).
fn combinations(names: &[String], k: usize) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    if k == 0 {
        out.push(Vec::new());
        return out;
    }
    if k == 1 {
        for n in names {
            out.push(vec![n.clone()]);
        }
        return out;
    }
    for i in 0..names.len() {
        for j in (i + 1)..names.len() {
            out.push(vec![names[i].clone(), names[j].clone()]);
        }
    }
    out
}

/// Methods connected by sharing at least one field — `_connected_groups`.
fn connected_groups(kept: &[String], mf: &[(String, HashSet<String>)]) -> Vec<Vec<String>> {
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for start in kept {
        if seen.contains(start.as_str()) {
            continue;
        }
        let mut group: Vec<String> = Vec::new();
        let mut stack: Vec<&str> = vec![start];
        while let Some(m) = stack.pop() {
            if !seen.insert(m) {
                continue;
            }
            group.push(m.to_string());
            let fields = mf
                .iter()
                .find(|(n, _)| n == m)
                .map(|(_, f)| f)
                .cloned()
                .unwrap_or_default();
            for other in kept {
                if seen.contains(other.as_str()) {
                    continue;
                }
                let other_fields = mf
                    .iter()
                    .find(|(n, _)| n == other)
                    .map(|(_, f)| f)
                    .cloned()
                    .unwrap_or_default();
                if !fields.is_disjoint(&other_fields) {
                    stack.push(other);
                }
            }
        }
        groups.push(group);
    }
    groups
}

/// `_partition_findings`: the latent-class field-partition family.
pub fn partition_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    for s in body {
        let Stmt::ClassDef(cls) = s else { continue };
        if let Some((connectors, groups)) = partition_for_class(source, cls) {
            let groups_text = groups
                .iter()
                .map(|g| format!("{{{}}}", g.join(",")))
                .collect::<Vec<_>>()
                .join("/");
            let conn_text = if connectors.is_empty() {
                "none".to_string()
            } else {
                format!("{{{}}}", connectors.join(","))
            };
            let metric: usize = groups.iter().map(|g| g.len()).sum();
            let _ = metric;
            state.findings.push(Finding {
                file: state.file.to_string(),
                line: line_of(source, cls.name.range().start()),
                function: cls.name.to_string(),
                kind: "partition".into(),
                severity: "fail".into(),
                message: format!(
                    "methods split into {} field-disjoint groups ({groups_text}), connectors removed: {conn_text} — each group touches only its own fields, so each is a latent class",
                    groups.len()
                ),
            });
        }
    }
}

const MONKEYPATCH_METHODS: [&str; 5] = ["setattr", "setitem", "delattr", "setenv", "delenv"];

fn monkeypatch_decorator(state: &mut ScanState, source: &str, d: &Decorator, mock_imports: &HashSet<String>) {
    let expr = &d.expression;
    let func = if let Expr::Call(c) = expr {
        c.func.as_ref()
    } else {
        expr
    };
    let desc: Option<String> = match func {
        Expr::Name(name) if mock_imports.contains(name.id.as_str()) => Some(format!("@{}", name.id.as_str())),
        Expr::Attribute(a) if a.attr.as_str() == "patch" => Some("@patch".to_string()),
        _ => None,
    };
    if let Some(desc) = desc {
        mp_finding(state, &desc, line_of(source, d.range().start()));
    }
}

/// Check whether an expression chain (Attribute or Name) resolves to an
/// imported mock symbol — guards against false positives on e.g. self.client.patch.
fn attr_resolves_to_mock(val: &Expr, imps: &HashSet<String>) -> bool {
    match val {
        Expr::Name(n) => imps.contains(n.id.as_str()),
        Expr::Attribute(inner) => {
            let seg = inner.attr.to_string();
            imps.contains(&seg) || attr_resolves_to_mock(&inner.value, imps)
        }
        _ => false,
    }
}

/// `_monkeypatch_findings` — forbidden patch usage in tests.
pub fn monkeypatch_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let mut mock_imports: HashSet<String> = HashSet::new();
    for s in body {
        match s {
            Stmt::ImportFrom(imp) => {
                if let Some(module) = &imp.module {
                    if module.to_string().contains("mock") {
                        for a in &imp.names {
                            mock_imports.insert(a.name.to_string());
                        }
                    }
                }
            }
            Stmt::Import(imp) => {
                for a in &imp.names {
                    if a.name.to_string().contains("mock") {
                        mock_imports.insert(a.name.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    // calls + decorators in one full walk
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            if let AnyNodeRef::ExprCall(call) = n {
                let line = line_of(source, call.range().start());
                let desc: Option<String> = match call.func.as_ref() {
                    Expr::Attribute(a) => match a.value.as_ref() {
                        Expr::Name(name)
                            if name.id == "monkeypatch" && MONKEYPATCH_METHODS.contains(&a.attr.as_str()) =>
                        {
                            Some(format!("monkeypatch.{}", a.attr.as_str()))
                        }
                        _ if a.attr.as_str() == "patch" && attr_resolves_to_mock(a.value.as_ref(), &mock_imports) => {
                            // Only flag .patch when the receiver chain resolves
                            // to an imported mock symbol — avoids false positives
                            // on e.g. self.client.patch(url)
                            Some(format!("{}.patch", &source[a.value.range()]))
                        }
                        _ => None,
                    },
                    Expr::Name(name) if name.id == "patch" && mock_imports.contains("patch") => {
                        Some("patch".to_string())
                    }
                    _ => None,
                };
                if let Some(desc) = desc {
                    mp_finding(state, &desc, line);
                }
            }
            // decorators: skel_children descends into the expression, never
            // the Decorator node — examine them at the function/class pop
            if let AnyNodeRef::StmtFunctionDef(f) = n {
                for d in &f.decorator_list {
                    monkeypatch_decorator(state, source, d, &mock_imports);
                }
            }
            if let AnyNodeRef::StmtClassDef(cls) = n {
                for d in &cls.decorator_list {
                    monkeypatch_decorator(state, source, d, &mock_imports);
                }
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
}

fn mp_finding(state: &mut ScanState, desc: &str, line: usize) {
    state.findings.push(Finding {
        file: state.file.to_string(),
        line,
        function: String::new(),
        kind: "monkeypatch".into(),
        severity: "fail".into(),
        message: format!(
            "{desc} at line {line} — never monkeypatch global state; inject an object fake (a class implementing the real protocol) via parameter injection or the services container — fakes are objects, not functions"
        ),
    });
}

const SKIPIF_NEEDLES: [&str; 5] = ["os.environ", "environ", "getenv", "os.path.exists", "sys.platform"];

/// `pytest.mark.skip` — the parked-test decorator (attribute chain only, so
/// unrelated `.skip` attributes are not flagged).
fn is_pytest_mark_skip(a: &ExprAttribute) -> bool {
    if a.attr.as_str() != "skip" {
        return false;
    }
    match a.value.as_ref() {
        Expr::Attribute(inner) => {
            inner.attr.as_str() == "mark" && matches!(inner.value.as_ref(), Expr::Name(n) if n.id.as_str() == "pytest")
        }
        _ => false,
    }
}

/// A parked-test finding — same family (kind `skipif`) and same message as
/// the env-needle case.
fn parked_skip_finding(state: &mut ScanState, source: &str, offset: ruff_text_size::TextSize) {
    let line = line_of(source, offset);
    state.findings.push(Finding {
        file: state.file.to_string(),
        line,
        function: String::new(),
        kind: "skipif".into(),
        severity: "fail".into(),
        message: format!("@pytest.mark.skip at line {line} — a permanently skipped test rots; fix it or delete it"),
    });
}

/// `_skipif_findings` — never skip a test: not for a missing environment
/// (`skipif` on an env needle) and not permanently (`@pytest.mark.skip` —
/// a parked test rots).
pub fn skipif_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    // `@pytest.mark.skip()` is both an ExprCall and (its func) an
    // ExprAttribute node — dedupe by decorator start so one decorator
    // reports at most once.
    let mut reported: HashSet<usize> = HashSet::new();
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            match n {
                AnyNodeRef::ExprCall(call) => {
                    if let Expr::Attribute(a) = call.func.as_ref() {
                        if a.attr.as_str() == "skipif" {
                            let cond_parts: Vec<String> = call
                                .arguments
                                .args
                                .iter()
                                .map(|e| source[e.range()].to_string())
                                .chain(
                                    call.arguments
                                        .keywords
                                        .iter()
                                        .map(|k| source[k.value.range()].to_string()),
                                )
                                .collect();
                            let cond = cond_parts.join(" ");
                            if SKIPIF_NEEDLES.iter().any(|needle| cond.contains(needle)) {
                                state.findings.push(Finding {
                                    file: state.file.to_string(),
                                    line: line_of(source, call.range().start()),
                                    function: String::new(),
                                    kind: "skipif".into(),
                                    severity: "fail".into(),
                                    message: format!(
                                        "@pytest.mark.skipif on environment presence at line {} — never skip a test for a missing dependency: fake it (a fixture builds a stand-in) so it runs identically everywhere; only the E2E suite may skip",
                                        line_of(source, call.range().start())
                                    ),
                                });
                            }
                        } else if a.attr.as_str() == "skip"
                            && is_pytest_mark_skip(a)
                            && call.arguments.args.is_empty()
                            && call.arguments.keywords.is_empty()
                            && reported.insert(call.range().start().to_usize())
                        {
                            parked_skip_finding(state, source, call.range().start());
                        }
                    }
                }
                AnyNodeRef::ExprAttribute(a)
                    if is_pytest_mark_skip(a) && reported.insert(a.range().start().to_usize()) =>
                {
                    // bare `@pytest.mark.skip` (no call parens) — the
                    // decorator expression is the attribute itself
                    parked_skip_finding(state, source, a.range().start());
                }
                _ => {}
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
}

const FS_PATH_METHODS: [&str; 17] = [
    "read_text",
    "write_text",
    "read_bytes",
    "write_bytes",
    "mkdir",
    "unlink",
    "rename",
    "replace",
    "touch",
    "rmdir",
    "iterdir",
    "glob",
    "rglob",
    "exists",
    "resolve",
    "symlink_to",
    "copy",
];
const FS_OS_OPS: [&str; 9] = [
    "remove", "rename", "mkdir", "makedirs", "rmdir", "unlink", "symlink", "link", "replace",
];
const FS_SHUTIL_OPS: [&str; 5] = ["copy", "copy2", "move", "rmtree", "copytree"];
const FS_TEMPFILE: [&str; 6] = [
    "TemporaryDirectory",
    "NamedTemporaryFile",
    "TemporaryFile",
    "mkdtemp",
    "mkstemp",
    "mktemp",
];

/// `_fakefs_findings` — real FS access in tests without pyfakefs.
// code-health: ignore large-function the fake-filesystem grammar is one decision table per backend
pub fn fakefs_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let uses_fakefs_base = {
        let mut base = false;
        for s in body {
            match s {
                Stmt::ImportFrom(imp) => {
                    if imp.module.as_ref().is_some_and(|m| m.to_string().contains("pyfakefs")) {
                        base = true;
                    }
                }
                Stmt::Import(imp) => {
                    if imp.names.iter().any(|a| a.name.to_string().contains("pyfakefs")) {
                        base = true;
                    }
                }
                Stmt::ClassDef(cls) => {
                    if let Some(arguments) = &cls.arguments {
                        for b in &arguments.args {
                            if source[b.range()].to_lowercase().contains("fakefs")
                                || source[b.range()].to_lowercase().contains("fake_filesystem")
                            {
                                base = true;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        base
    };
    // all functions, with their enclosing test-class status
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            if let AnyNodeRef::StmtFunctionDef(f) = n {
                let line = line_of(source, f.name.range().start());
                let is_test = f.name.as_str().starts_with("test_")
                    || body.iter().any(|s| {
                        matches!(s, Stmt::ClassDef(c) if c.name.to_lowercase().starts_with("test")
                            && c.body.iter().any(|m| m.range().start() == n.range().start()))
                    });
                if !is_test {
                    skel_children(n, &mut queue);
                    qi += 1;
                    continue;
                }
                let uses_fakefs = f
                    .parameters
                    .posonlyargs
                    .iter()
                    .chain(&f.parameters.args)
                    .any(|p| p.parameter.name.as_str() == "fs");
                if uses_fakefs || uses_fakefs_base {
                    skel_children(n, &mut queue);
                    qi += 1;
                    continue;
                }
                // real-FS usage vs sanctioned real-FS need
                let mut real_fs = false;
                let mut needs_real = false;
                // ast.walk includes decorators + parameter annotations —
                // a skipif guard on a real file (.exists()) is FS usage
                let mut fq: Vec<Q> = f.body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
                for d in &f.decorator_list {
                    fq.push(Q::N(AnyNodeRef::from(&d.expression)));
                }
                let mut fi = 0usize;
                while fi < fq.len() {
                    if let Q::N(fn_) = fq[fi] {
                        match fn_ {
                            AnyNodeRef::ExprName(n) if n.id == "tmp_path" => real_fs = true,
                            AnyNodeRef::ExprName(n) if matches!(n.id.as_str(), "sqlite3" | "subprocess") => {
                                needs_real = true
                            }
                            AnyNodeRef::ExprAttribute(a)
                                if matches!(a.attr.as_str(), "symlink" | "symlink_to" | "link") =>
                            {
                                needs_real = true
                            }
                            AnyNodeRef::ExprCall(c) => {
                                match c.func.as_ref() {
                                    // bare open(...)/tempfile(...) count; a Path(...).open(...)
                                    // attribute call does NOT (Python's _real_fs_usage: only
                                    // Name funcs match open/tempfile; attribute calls must be
                                    // in the path/os/shutil/tempfile method lists)
                                    Expr::Name(n) if matches!(n.id.as_str(), "open" | "tempfile") => {
                                        real_fs = true;
                                    }
                                    Expr::Attribute(a) => {
                                        let attr = a.attr.as_str();
                                        if FS_PATH_METHODS.contains(&attr)
                                            || FS_OS_OPS.contains(&attr)
                                            || FS_SHUTIL_OPS.contains(&attr)
                                            || FS_TEMPFILE.contains(&attr)
                                        {
                                            real_fs = true;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                        skel_children(fn_, &mut fq);
                    }
                    fi += 1;
                }
                if real_fs && !needs_real {
                    state.findings.push(Finding {
                        file: state.file.to_string(),
                        line,
                        function: f.name.to_string(),
                        kind: "fakefs".into(),
                        severity: "fail".into(),
                        message: format!(
                            "test '{}' at line {line} touches the real filesystem (tmp_path/open/Path) without pyfakefs — tests fake the filesystem (the `fs` fixture or fake_filesystem_unittest). Reach a real tmp_path only when the code under test needs real FS semantics (subprocess interop, symlinks, C-level I/O like sqlite3) and comment why — or mark `# code-health: ignore-file fakefs <why>`",
                            f.name.as_str()
                        ),
                    });
                }
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
}

/// Walk a test function's full body (nested functions included) for an
/// assertion: an `assert` statement or a call to `pytest.raises` /
/// `pytest.fail` / bare `fail(`.
fn has_assertion(body: &[Stmt]) -> bool {
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            match n {
                AnyNodeRef::StmtAssert(_) => return true,
                AnyNodeRef::ExprCall(call) => match call.func.as_ref() {
                    Expr::Attribute(a) => {
                        if a.attr.as_str() == "raises" || a.attr.as_str() == "fail" {
                            return true;
                        }
                    }
                    Expr::Name(nm) if nm.id.as_str() == "fail" => return true,
                    _ => {}
                },
                _ => {}
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
    false
}

/// A function named `test_*` whose body contains no assertion can never
/// fail — it is a test that tests nothing.
pub fn no_assert_test_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            if let AnyNodeRef::StmtFunctionDef(f) = n {
                if f.name.as_str().starts_with("test_") && !has_assertion(&f.body) {
                    state.findings.push(Finding {
                        file: state.file.to_string(),
                        line: line_of(source, f.name.range().start()),
                        function: f.name.to_string(),
                        kind: "no-assert-test".into(),
                        severity: "fail".into(),
                        message: "test has no assertion — it can never fail".to_string(),
                    });
                }
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
}

// =====================================================================
// over-abstraction: an ABC with exactly one concrete subclass is ceremony
// (the last Python finding family — cross-file class + import resolution)
// =====================================================================

const ABSTRACT_DECORATORS: [&str; 4] = [
    "abstractmethod",
    "abstractproperty",
    "abstractclassmethod",
    "abstractstaticmethod",
];

/// Top-level classes with abstractness and base references.
pub fn collect_classes(body: &[Stmt], source: &str) -> Vec<crate::ClassInfo> {
    let mut out = Vec::new();
    for s in body {
        let Stmt::ClassDef(cls) = s else { continue };
        let mut abstract_ = false;
        for d in &cls.decorator_list {
            let expr = if let Expr::Call(c) = &d.expression {
                c.func.as_ref()
            } else {
                &d.expression
            };
            match expr {
                Expr::Name(n) if ABSTRACT_DECORATORS.contains(&n.id.as_str()) => abstract_ = true,
                Expr::Attribute(a) if ABSTRACT_DECORATORS.contains(&a.attr.as_str()) => abstract_ = true,
                _ => {}
            }
        }
        let mut bases: Vec<String> = Vec::new();
        if let Some(arguments) = &cls.arguments {
            for b in &arguments.args {
                match b {
                    Expr::Name(n) => {
                        if matches!(n.id.to_lowercase().as_str(), "abc" | "abcmeta") {
                            abstract_ = true;
                        }
                        bases.push(format!("Name:{}", n.id.as_str()));
                    }
                    Expr::Attribute(a) => {
                        if matches!(a.attr.to_lowercase().as_str(), "abc" | "abcmeta") {
                            abstract_ = true;
                        }
                        if let Expr::Name(v) = a.value.as_ref() {
                            bases.push(format!("Attr:{}:{}", v.id.as_str(), a.attr.as_str()));
                        }
                    }
                    _ => {}
                }
            }
        }
        out.push(crate::ClassInfo {
            name: cls.name.to_string(),
            line: line_of(source, cls.name.range().start()),
            abstract_,
            bases,
        });
    }
    out
}

/// Import aliases from top-level imports — `_import_map`.
pub fn collect_imports(body: &[Stmt], _source: &str) -> Vec<crate::ImportInfo> {
    let mut out = Vec::new();
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            match n {
                AnyNodeRef::StmtImportFrom(imp) => {
                    if let Some(module) = &imp.module {
                        for a in &imp.names {
                            let alias = a
                                .asname
                                .as_ref()
                                .map(|x| x.to_string())
                                .unwrap_or_else(|| a.name.to_string());
                            out.push(crate::ImportInfo {
                                alias,
                                module: module.to_string(),
                                imported: a.name.to_string(),
                            });
                        }
                    }
                }
                AnyNodeRef::StmtImport(imp) => {
                    for a in &imp.names {
                        let alias = a
                            .asname
                            .as_ref()
                            .map(|x| x.to_string())
                            .unwrap_or_else(|| a.name.to_string());
                        out.push(crate::ImportInfo {
                            alias,
                            module: a.name.to_string(),
                            imported: a.name.to_string(),
                        });
                    }
                }
                _ => {}
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
    out
}

/// `_class_key`: a module reference to a file rel.
fn class_key(
    classes: &std::collections::HashMap<(String, String), &crate::ClassInfo>,
    mrel: &str,
    mname: &str,
) -> Option<(String, String)> {
    if mrel.ends_with(".py") {
        let key = (mrel.to_string(), mname.to_string());
        return if classes.contains_key(&key) { Some(key) } else { None };
    }
    let base = mrel.replace('.', "/");
    for candidate in [format!("{base}.py"), format!("{base}/__init__.py")] {
        let key = (candidate, mname.to_string());
        if classes.contains_key(&key) {
            return Some(key);
        }
    }
    None
}

/// Abstract classes with exactly one concrete subclass — `_abstraction_actions`.
pub fn abstraction_findings(scans: &[(String, Vec<crate::ClassInfo>, Vec<crate::ImportInfo>)]) -> Vec<Finding> {
    use std::collections::HashMap;
    let mut classes: HashMap<(String, String), &crate::ClassInfo> = HashMap::new();
    for (rel, cls_list, _) in scans {
        for c in cls_list {
            classes.insert((rel.clone(), c.name.clone()), c);
        }
    }
    let mut concrete: HashMap<(String, String), Vec<String>> = HashMap::new();
    for (rel, cls_list, imports) in scans {
        let import_map: HashMap<&str, (&str, &str)> = imports
            .iter()
            .map(|i| (i.alias.as_str(), (i.module.as_str(), i.imported.as_str())))
            .collect();
        for c in cls_list {
            for base in &c.bases {
                let candidates: Vec<(String, String)> = if let Some(rest) = base.strip_prefix("Name:") {
                    let mut cands = vec![(rel.clone(), rest.to_string())];
                    if let Some((module, _name)) = import_map.get(rest) {
                        cands.push(((*module).to_string(), rest.to_string()));
                    }
                    cands
                } else if let Some(rest) = base.strip_prefix("Attr:") {
                    let mut parts = rest.splitn(2, ':');
                    let alias = parts.next().unwrap_or("");
                    let attr = parts.next().unwrap_or("");
                    let mut cands = Vec::new();
                    if let Some((module, _name)) = import_map.get(alias) {
                        cands.push(((*module).to_string(), attr.to_string()));
                    }
                    cands
                } else {
                    Vec::new()
                };
                for (mrel, mname) in candidates {
                    if let Some(key) = class_key(&classes, &mrel, &mname) {
                        if key != (rel.clone(), c.name.clone())
                            && classes.get(&key).map(|k| k.abstract_).unwrap_or(false)
                        {
                            concrete.entry(key).or_default().push(c.name.clone());
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut entries: Vec<((String, String), Vec<String>)> = concrete.into_iter().collect();
    entries.sort();
    for ((rel, name), subs) in entries {
        if subs.len() == 1 {
            let line = classes.get(&(rel.clone(), name.clone())).map(|c| c.line).unwrap_or(1);
            out.push(Finding {
                file: rel.clone(),
                line,
                function: name.clone(),
                kind: "over-abstraction".into(),
                severity: "fail".into(),
                message: format!(
                    "abstract class '{name}' in {rel} has exactly one concrete subclass ('{}') — an ABC with a single implementation is ceremony: fold the subclass into the base or drop the ABC; an abstraction earns its keep at two real, differing implementations",
                    subs[0]
                ),
            });
        }
    }
    out
}

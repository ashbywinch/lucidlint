//! The remaining standard-family checks, mirroring the Python implementation
//! exactly: suppressions, type-ignore, global-state, builtin-shadow, closures,
//! class-module, vague-name, strewing, except-swallows, broad-except.

use ruff_python_ast::Expr;
use ruff_python_ast::Stmt;
use ruff_python_ast::token::{Token, TokenKind, Tokens};
use ruff_text_size::Ranged;
use std::collections::HashSet;

use crate::{Finding, FnScope, ScanState, line_of, stmt_line};

pub const VAGUE_SUFFIXES: [&str; 8] = [
    "Manager", "Orchestrator", "Handler", "Store", "Repository", "Controller", "Utils", "Info",
];

pub const SHADOWED_BUILTINS: &[&str] = &[
    "abs", "all", "any", "bin", "bool", "bytes", "callable", "chr", "classmethod", "complex",
    "dict", "dir", "divmod", "enumerate", "eval", "exec", "filter", "float", "format",
    "frozenset", "getattr", "globals", "hasattr", "hash", "hex", "id", "input", "int",
    "isinstance", "issubclass", "iter", "len", "list", "locals", "map", "max", "memoryview",
    "min", "next", "object", "oct", "open", "ord", "pow", "print", "property", "range", "repr",
    "reversed", "round", "set", "setattr", "slice", "sorted", "staticmethod", "str", "sum",
    "super", "tuple", "type", "vars", "zip",
];

/// One line-level suppression and the (signal, why) parsed from it.
pub struct Suppressions {
    /// line -> (signal, why) — a finding on that line or line-1 is exempt.
    pub line: std::collections::HashMap<usize, (String, String)>,
    /// signal -> why — file-scoped exemptions.
    pub file: std::collections::HashMap<String, String>,
}

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

/// Parse `# code-health: ignore <signal> <why>` / `ignore-file` comments.
pub fn parse_suppressions(source: &str, tokens: &Tokens) -> Suppressions {
    let mut line_map = std::collections::HashMap::new();
    let mut file_map = std::collections::HashMap::new();
    for (ln, text) in comment_lines(source, tokens) {
        let trimmed = text.trim_start_matches('#').trim_start();
        if let Some(rest) = trimmed.strip_prefix("code-health: ignore-file ") {
            let mut it = rest.splitn(2, char::is_whitespace);
            let signal = it.next().unwrap_or("").to_string();
            let why = it.next().unwrap_or("").trim().to_string();
            if !signal.is_empty() {
                file_map.insert(signal, why);
            }
        } else if let Some(rest) = trimmed.strip_prefix("code-health: ignore ") {
            let mut it = rest.splitn(2, char::is_whitespace);
            let signal = it.next().unwrap_or("").to_string();
            let why = it.next().unwrap_or("").trim().to_string();
            if !signal.is_empty() {
                line_map.insert(ln, (signal, why));
            }
        }
    }
    Suppressions {
        line: line_map,
        file: file_map,
    }
}

/// The Python `_suppressed`: a finding is exempt when its line or line-1
/// carries an explained suppression for that signal.
fn suppressed(signal: &str, line: usize, supps: &Suppressions) -> bool {
    for ln in [line, line.saturating_sub(1)] {
        if let Some((sig, why)) = supps.line.get(&ln) {
            if sig == signal && !why.is_empty() {
                return true;
            }
        }
    }
    false
}

/// Filter findings through the suppressions + emit the why-less suppression
/// findings, mirroring `_scan_file`'s post-filter.
pub fn apply_suppressions(
    findings: Vec<Finding>,
    source: &str,
    file: &str,
    tokens: &Tokens,
) -> Vec<Finding> {
    let supps = parse_suppressions(source, tokens);
    let mut out = Vec::new();
    // the Python tool dedups suppressions by line (one per line)
    let mut seen_invalid: HashSet<usize> = HashSet::new();
    for (ln, (sig, why)) in &supps.line {
        if why.is_empty() && seen_invalid.insert(*ln) {
            out.push(Finding {
                file: file.to_string(),
                line: *ln,
                function: String::new(),
                kind: "suppression".into(),
                severity: "fail".into(),
                message: format!(
                    "suppression '# code-health: ignore {sig}' at line {ln} without a why — exemptions only apply with an explanation"
                ),
            });
        }
    }
    for (sig, why) in &supps.file {
        if why.is_empty() {
            // the Python emits one finding per invalid ignore-file line; we
            // approximate with the first line carrying it
            if let Some((ln, _)) = comment_lines(source, tokens)
                .iter()
                .find(|(_, t)| t.contains(&format!("code-health: ignore-file {sig}")))
            {
                out.push(Finding {
                    file: file.to_string(),
                    line: *ln,
                    function: String::new(),
                    kind: "suppression".into(),
                    severity: "fail".into(),
                    message: format!(
                        "file suppression '# code-health: ignore-file {sig}' at line {ln} without a why — exemptions only apply with an explanation"
                    ),
                });
            }
        }
    }
    for f in findings {
        if suppressed(&f.kind, f.line, &supps) {
            continue;
        }
        if let Some(why) = supps.file.get(&f.kind) {
            if !why.is_empty() {
                continue; // ignore-file with a why exempts; a why-less one does not
            }
        }
        out.push(f);
    }
    out
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

/// Module-level mutable literals, Global statements, and mutations of module
/// containers inside functions — mirrors the dispatcher's global-state handlers.
pub fn global_state_findings(state: &mut ScanState, stmt: &Stmt, module_level: bool) {
    let fn_name = state
        .current_fn
        .as_ref()
        .map(|f| f.0.clone())
        .unwrap_or_default();
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
                && d.items.iter().all(|it| {
                    it.key.as_ref().map(all_constant).unwrap_or(false) && all_constant(&it.value)
                })
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
                    message: format!(
                        "parameter '{}' shadows a builtin — rename it",
                        v.name.as_str()
                    ),
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
                    message: format!(
                        "parameter '{}' shadows a builtin — rename it",
                        k.name.as_str()
                    ),
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
                    let fn_name = state
                        .current_fn
                        .as_ref()
                        .map(|f| f.0.clone())
                        .unwrap_or_default();
                    state.findings.push(Finding {
                        file: state.file.to_string(),
                        line: stmt_line(state.source, stmt),
                        function: fn_name,
                        kind: "builtin-shadow".into(),
                        severity: "fail".into(),
                        message: format!(
                            "variable '{}' shadows a builtin — rename it",
                            n.id.as_str()
                        ),
                    });
                }
            }
        }
    }
}

/// The except family: swallows (fail) and broad excepts (warn).
pub fn except_findings(state: &mut ScanState, stmt: &Stmt) {
    let Stmt::Try(t) = stmt else { return };
    let returned: HashSet<String> = state
        .fn_stack
        .last()
        .map(|s| s.returned.clone())
        .unwrap_or_default();
    let fn_name = state
        .current_fn
        .as_ref()
        .map(|f| f.0.clone())
        .unwrap_or_default();
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
            let kind = if type_opt.is_none() { "bare except" } else { "except that swallows" };
            state.findings.push(Finding {
                file: state.file.to_string(),
                line,
                function: fn_name.clone(),
                kind: "except".into(),
                severity: "fail".into(),
                message: format!(
                    "{kind} at line {line} — the catch never raises, returns, or surfaces the error; re-raise or mark `# code-health: ignore except <why>`"
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
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {}
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

fn walk_handler(
    stmt: &Stmt,
    exits: &mut bool,
    process_exit: &mut bool,
    returned: &HashSet<String>,
) {
    // process exit / returned-name mutation detection via a manual walk
    let mut stack: Vec<&Stmt> = vec![stmt];
    while let Some(s) = stack.pop() {
        if matches!(s, Stmt::Return(_) | Stmt::Raise(_) | Stmt::Break(_) | Stmt::Continue(_)) {
            *exits = true;
            return;
        }
        if let Stmt::FunctionDef(f) = s {
            for body_stmt in &f.body {
                stack.push(body_stmt);
            }
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
                matches!(a.attr.as_str(), "exit")
                    && matches!(a.value.as_ref(), Expr::Name(n) if n.id.as_str() == "sys")
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
                if let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h {
                    for b in &eh.body {
                        stack.push(b);
                    }
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
    let classes: Vec<&Stmt> = module_body
        .iter()
        .filter(|s| matches!(s, Stmt::ClassDef(_)))
        .collect();
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
            let methods = cls
                .body
                .iter()
                .filter(|m| matches!(m, Stmt::FunctionDef(_)))
                .count();
            let span = line_of(state.source, cls.range().end())
                .saturating_sub(line_of(state.source, cls.range().start()));
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
    let mut groups: std::collections::HashMap<String, Vec<(String, usize)>> =
        std::collections::HashMap::new();
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
        let names: Vec<String> = members
            .iter()
            .map(|(n, l)| format!("{n} (line {l})"))
            .collect();
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
            Expr::Name(n) if state.module_mutables.contains(n.id.as_str()) => {
                Some(n.id.to_string())
            }
            Expr::Subscript(s) if state.module_mutables.contains(sub_name(s).as_str()) => {
                Some(sub_name(s))
            }
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

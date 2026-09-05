// lucidlint: ignore-file complexity the parity-locked AST walkers are single dispatch tables —
// lucidlint: ignore-file boolean-arg Rust has no named arguments — the traversal's
// positional in_call_func flag is the API contract, not an unnamed boolean
// match-arm count is table size, not branching; keep NEW functions under cc 15

//! The remaining standard-family checks, mirroring the Python implementation
//! exactly: suppressions, type-ignore, global-state, builtin-shadow, closures,
//! class-module, vague-name, strewing, except-swallows, broad-except.

use rayon::prelude::*;
use ruff_python_ast::token::{TokenKind, Tokens};
use ruff_python_ast::{
    AnyNodeRef, BoolOp, CmpOp, Decorator, Expr, ExprAttribute, ExprCall, ExprContext, Operator, Parameters, Pattern,
    Stmt, StmtClassDef, StmtFunctionDef, UnaryOp,
};
use ruff_text_size::Ranged;
use std::collections::HashSet;

use crate::{col_of, line_of, stmt_line, Finding, ScanState};

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
    // tokens are in source order — count newlines between comment starts
    // instead of re-scanning from offset 0 per comment (`line_of` is O(n);
    // N comments used to cost O(N^2) — a comment-heavy file scanned 4x per
    // run and went quadratic)
    let mut line = 1usize;
    let mut prev = 0usize;
    for tok in tokens.iter() {
        if tok.kind() == TokenKind::Comment {
            let range = tok.range();
            let start = range.start().to_usize();
            line += source[prev..start].bytes().filter(|&b| b == b'\n').count();
            prev = start;
            let text = &source[range];
            out.push((line, text.to_string()));
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
    books: &mut crate::common::SuppressionBooks,
) -> Vec<Finding> {
    crate::common::apply_suppressions_impl(findings, &comment_lines(source, tokens), file, "#", books)
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
col: 0,
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
/// Does an `# noqa` / `# pragma: no cover` comment carry a reason?
/// `rest` is everything after the marker. Accepts the house format — a
/// reason on the SAME comment line (``# noqa: BLE001 — reason``, a second
/// `#` comment, or bare prose) — and still rejects the code-only form
/// (``# noqa: BLE001`` with nothing after the code).
fn noqa_reason(rest: &str) -> bool {
    let rest = rest.trim_start();
    let rest = match rest.strip_prefix(':') {
        Some(r) => {
            // `: CODE [reason]` — the code is the first token; a reason must
            // follow it. `# noqa: BLE001` (no reason) leaves nothing.
            let r = r.trim_start();
            match r.find(char::is_whitespace) {
                Some(i) => r[i..].trim(),
                None => "",
            }
        }
        None => rest, // bare `# noqa` / prose — accept a prose reason as-is
    };
    !rest.is_empty()
}

pub fn noqa_findings(source: &str, file: &str, tokens: &Tokens) -> Vec<Finding> {
    let mut out = Vec::new();
    for (ln, text) in comment_lines(source, tokens) {
        let marker = if text.contains("# noqa") {
            "# noqa"
        } else if text.contains("# pragma: no cover") {
            "# pragma: no cover"
        } else {
            continue;
        };
        let rest = text.split_once(marker).map(|(_, r)| r).unwrap_or("");
        if !noqa_reason(rest) {
            out.push(Finding {
                col: 0,
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
col: 0,
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
                            col: 0,
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
                    col: 0,
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
                    col: 0,
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
                    col: 0,
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
                    col: 0,
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
                    col: 0,
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
                        col: 0,
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
    state.findings.push(Finding {
col: 0,
        file: state.file.to_string(),
        line: line_of(source, call.range().start()),
        function: fn_name,
        kind: "positional-literals".into(),
        severity: "warn".into(),
        message: format!(
            "call passes {n} {kind} positionally to {callee}() — a swapped argument is a silent bug; use keyword arguments{directive}",
            callee = callee_name,
        ),
    });
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
    state.findings.push(Finding {
col: 0,
        file: state.file.to_string(),
        line,
        function: fn_name.clone(),
        kind: "detached-method".into(),
        severity: "warn".into(),
        message: format!(
            "method '{fn_name}' never uses '{recv}' — it does not touch instance state; make it a @staticmethod or move it out of the class"
        ),
    });
}

/// Does any expression in the body reference the given name? A source-order
/// walk — nested functions included (a closure capturing `self` still uses
/// instance state).
fn body_refs_name(body: &[Stmt], name: &str) -> bool {
    use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
    struct Probe<'a> {
        name: &'a str,
        found: bool,
    }
    impl<'a> SourceOrderVisitor<'a> for Probe<'a> {
        fn visit_expr(&mut self, e: &'a Expr) {
            if let Expr::Name(n) = e {
                if n.id.as_str() == self.name {
                    self.found = true;
                }
            }
            walk_stmt_probe(self, e);
        }
    }
    // walk_expr is the trait default; we must recurse manually
    fn walk_stmt_probe<'a, V: SourceOrderVisitor<'a>>(v: &mut V, e: &'a Expr) {
        ruff_python_ast::visitor::source_order::walk_expr(v, e);
    }
    let mut probe = Probe { name, found: false };
    for s in body {
        walk_stmt(&mut probe, s);
        if probe.found {
            break;
        }
    }
    probe.found
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
        state.findings.push(Finding {
            col: 0,
            file: state.file.to_string(),
            line: line_of(source, f.name.range().start()),
            function: f.name.to_string(),
            kind: "long-param-list".into(),
            severity: "fail".into(),
            message: format!(
                "{n} parameters — introduce a parameter object named with a domain noun — fix: long-param-list --fix-name <Options>"
            ),
        });
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
col: 0,
                file: state.file.to_string(),
                line,
                function: fn_name.clone(),
                kind: "swallow".into(),
                severity: "fail".into(),
                message: format!(
                    "{kind} at line {line} — logs are not surfacing: a caller exists that needs to decide; surface by return, raise, break, continue, sys.exit, or mutating a name the enclosing function returns — mark `# lucidlint: ignore swallow <terminal-boundary reason>` only when no caller exists to propagate to"
                ),
            });
        } else if let Some(ty) = type_opt {
            let base = annotation_base_name(ty);
            if matches!(base.as_deref(), Some("Exception") | Some("BaseException")) {
                state.findings.push(Finding {
col: 0,
                    file: state.file.to_string(),
                    line: line_of(state.source, eh.range().start()),
                    function: fn_name.clone(),
                    kind: "broad-except".into(),
                    severity: "warn".into(),
                    message: "broad except Exception - catch what you actually handle; a true boundary catch states its blast radius in the why".into(),
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

pub fn stmt_exprs(s: &Stmt) -> Vec<&Expr> {
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

/// Closures: >= 2 inner functions/lambdas with cc >= 15, span >= 60, OR
/// shared-state mutation (the accumulator pattern — closures that WRITE to
/// the enclosing function's locals are a class in disguise even when small).
pub fn closure_findings(state: &mut ScanState, stmt: &Stmt, cc: u32, span: u32) {
    let Stmt::FunctionDef(f) = stmt else { return };
    let inner = inner_function_count(stmt);
    if inner < 2 {
        return;
    }
    let mutated = closures_mutate_shared_state(f);
    if cc < 15 && span < 60 && mutated.is_empty() {
        return;
    }
    let line = stmt_line(state.source, stmt);
    let why = if mutated.is_empty() {
        String::new()
    } else {
        format!(
            " — its closures write to {} captured local{} (the accumulator pattern)",
            mutated.len(),
            if mutated.len() == 1 { "" } else { "s" }
        )
    };
    state.findings.push(Finding {
        col: 0,
        file: state.file.to_string(),
        line,
        function: f.name.to_string(),
        kind: "closures".into(),
        severity: "fail".into(),
        message: format!(
            "'{}' defines {inner} inner functions closing over its state{why} — a class in disguise",
            f.name.as_str()
        ),
    });
}

/// Method calls that mutate the receiver in place — the writes that turn a
/// captured enclosing-scope local into shared instance state.
const MUTATING_METHODS: &[&str] = &[
    "append",
    "extend",
    "insert",
    "pop",
    "remove",
    "clear",
    "sort",
    "reverse",
    "add",
    "discard",
    "update",
    "setdefault",
    "appendleft",
    "appendright",
    "put",
    "push",
    "enqueue",
    "__setitem__",
    "__iadd__",
];

/// The distinct enclosing-scope locals that the direct inner functions WRITE
/// to (mutating method calls, subscript/attribute stores, nonlocal rebinds).
/// Empty = the closures only read the enclosing state (a factory of handlers
/// is the legit idiom; a class in disguise is not). Only DIRECT inner
/// functions are scanned — a deeper nesting is judged by its own
/// `closure_findings` run when its enclosing function is scanned.
pub fn closures_mutate_shared_state(f: &StmtFunctionDef) -> Vec<String> {
    let (inners, outer_bound) = scope_fns_and_bindings(&f.body);
    let mut params = HashSet::new();
    function_param_names(&f.parameters, &mut params);
    let mut mutated: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for inner in inners {
        let mut inner_bound = HashSet::new();
        function_param_names(&inner.parameters, &mut inner_bound);
        let (_, deeper) = scope_fns_and_bindings(&inner.body);
        inner_bound.extend(deeper);
        let candidates: HashSet<&str> = outer_bound
            .union(&params)
            .filter(|n| !inner_bound.contains(*n))
            .map(|s| s.as_str())
            .collect();
        for name in &candidates {
            if !seen.contains(*name) && scope_mutates(&inner.body, &HashSet::from([*name])) {
                seen.insert(name.to_string());
                mutated.push(name.to_string());
            }
        }
    }
    mutated
}

/// Does any statement in *body* (control-flow descend, not nested def/class
/// scopes) WRITE to one of *candidates*?
fn scope_mutates(body: &[Stmt], candidates: &HashSet<&str>) -> bool {
    let mut stack: Vec<&Stmt> = Vec::new();
    for s in body {
        stack.push(s);
    }
    while let Some(s) = stack.pop() {
        if stmt_writes(s, candidates) {
            return true;
        }
        if matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            continue; // a new scope — its bodies belong to a deeper run
        }
        push_stmt_children(s, &mut stack);
    }
    false
}

fn stmt_writes(s: &Stmt, candidates: &HashSet<&str>) -> bool {
    match s {
        Stmt::Assign(a) => a.targets.iter().any(|t| store_target_writes(t, candidates)),
        Stmt::AnnAssign(a) => store_target_writes(&a.target, candidates),
        Stmt::AugAssign(a) => store_target_writes(&a.target, candidates),
        Stmt::Delete(d) => d.targets.iter().any(|t| store_target_writes(t, candidates)),
        Stmt::Nonlocal(n) => n.names.iter().any(|id| candidates.contains(id.as_str())),
        Stmt::Expr(e) => expr_writes(&e.value, candidates),
        Stmt::Return(r) => r.value.as_ref().is_some_and(|v| expr_writes(v, candidates)),
        Stmt::If(i) => expr_writes(&i.test, candidates),
        Stmt::While(w) => expr_writes(&w.test, candidates),
        Stmt::For(f) => expr_writes(&f.iter, candidates) || expr_writes(&f.target, candidates),
        Stmt::With(w) => w.items.iter().any(|item| {
            expr_writes(&item.context_expr, candidates)
                || item.optional_vars.as_ref().is_some_and(|v| expr_writes(v, candidates))
        }),
        Stmt::Try(t) => t.handlers.iter().any(|h| match h {
            ruff_python_ast::ExceptHandler::ExceptHandler(eh) => {
                eh.type_.as_ref().is_some_and(|t| expr_writes(t, candidates))
            }
        }),
        Stmt::Assert(a) => expr_writes(&a.test, candidates),
        _ => false,
    }
}

/// An assignment/delete TARGET that writes INTO a candidate container
/// (L[0] = x, L.attr = x, del L[i]) — plain name rebinds are not writes to
/// the object (they need `nonlocal`, handled separately).
fn store_target_writes(t: &Expr, candidates: &HashSet<&str>) -> bool {
    match t {
        Expr::Name(_) => false,
        Expr::Subscript(s) => {
            let mut v = s.value.as_ref();
            loop {
                match v {
                    Expr::Subscript(inner) => v = inner.value.as_ref(),
                    Expr::Name(n) => return candidates.contains(n.id.as_str()),
                    _ => return false,
                }
            }
        }
        Expr::Attribute(a) => store_target_writes(&a.value, candidates),
        Expr::Tuple(t) => t.elts.iter().any(|e| store_target_writes(e, candidates)),
        Expr::List(l) => l.elts.iter().any(|e| store_target_writes(e, candidates)),
        Expr::Starred(st) => store_target_writes(&st.value, candidates),
        _ => false,
    }
}

/// Does *e* WRITE to a candidate: a mutating method call on it, or a write
/// nested anywhere inside (call args, subscripts, comprehensions, lambdas).
fn expr_writes(e: &Expr, candidates: &HashSet<&str>) -> bool {
    // the unified traversal descends every child; this closure only decides
    // whether a CALL mutates a captured candidate (the receiver check)
    let mut found = false;
    walk_expr_deep(e, false, &mut |x, _| {
        if found {
            return;
        }
        if let Expr::Call(c) = x {
            if let Expr::Attribute(a) = c.func.as_ref() {
                // receiver mutation: L.append(...) or writer.lines.append(...)
                // — peel attribute chains to the base name before the
                // candidates check
                if MUTATING_METHODS.contains(&a.attr.as_str()) {
                    let mut receiver = a.value.as_ref();
                    loop {
                        match receiver {
                            Expr::Attribute(inner) => receiver = inner.value.as_ref(),
                            Expr::Name(n) => {
                                if candidates.contains(n.id.as_str()) {
                                    found = true;
                                }
                                break;
                            }
                            _ => break,
                        }
                    }
                }
            }
        }
    });
    found
}

/// The direct inner functions of *body*'s scope (control-flow nesting only —
/// a def inside a nested def/class belongs to that nested scope) and the
/// names the scope binds at any control-flow depth.
fn scope_fns_and_bindings(body: &[Stmt]) -> (Vec<&StmtFunctionDef>, HashSet<String>) {
    let mut fns = Vec::new();
    let mut bound: HashSet<String> = HashSet::new();
    let mut stack: Vec<&Stmt> = Vec::new();
    for s in body {
        stack.push(s);
    }
    while let Some(s) = stack.pop() {
        match s {
            Stmt::FunctionDef(f) => {
                fns.push(f);
                bound.insert(f.name.to_string());
            }
            Stmt::ClassDef(c) => {
                bound.insert(c.name.to_string());
            }
            Stmt::Assign(a) => {
                for t in &a.targets {
                    expr_bindings(t, &mut bound);
                }
            }
            Stmt::AnnAssign(a) => expr_bindings(&a.target, &mut bound),
            Stmt::AugAssign(a) => expr_bindings(&a.target, &mut bound),
            Stmt::For(f) => {
                expr_bindings(&f.target, &mut bound);
            }
            Stmt::With(w) => {
                for item in &w.items {
                    if let Some(v) = &item.optional_vars {
                        expr_bindings(v, &mut bound);
                    }
                }
            }
            Stmt::Import(i) => {
                for a in &i.names {
                    bound.insert(a.name.as_str().split('.').next().unwrap_or("").to_string());
                }
            }
            Stmt::ImportFrom(i) => {
                for a in &i.names {
                    if a.name.as_str() != "*" {
                        bound.insert(a.name.to_string());
                    }
                }
            }
            _ => {}
        }
        if matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            continue; // nested scopes are not this scope's bindings
        }
        push_stmt_children(s, &mut stack);
    }
    (fns, bound)
}

/// Names an expression binds as an assignment/unpacking target.
fn expr_bindings(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Name(n) => {
            out.insert(n.id.to_string());
        }
        Expr::Tuple(t) => {
            for el in &t.elts {
                expr_bindings(el, out);
            }
        }
        Expr::List(l) => {
            for el in &l.elts {
                expr_bindings(el, out);
            }
        }
        Expr::Starred(st) => expr_bindings(&st.value, out),
        _ => {}
    }
}

/// The parameter names of a function (posonly, args, kwonly, vararg, kwarg).
fn function_param_names(p: &Parameters, out: &mut HashSet<String>) {
    for a in &p.posonlyargs {
        out.insert(a.parameter.name.to_string());
    }
    for a in &p.args {
        out.insert(a.parameter.name.to_string());
    }
    for a in &p.kwonlyargs {
        out.insert(a.parameter.name.to_string());
    }
    if let Some(v) = &p.vararg {
        out.insert(v.name.to_string());
    }
    if let Some(k) = &p.kwarg {
        out.insert(k.name.to_string());
    }
}

// ------------------------------------------------------------- latent-class
// cross-function shapes (module scope): a method in exile, an anonymous
// tuple record, and a state-threading assembly site.

/// The `self.<attr> = ...` assignments across a class's methods.
fn class_attr_names(class_body: &[Stmt]) -> HashSet<String> {
    let mut attrs = HashSet::new();
    for m in class_body {
        let Stmt::FunctionDef(f) = m else { continue };
        for s in &f.body {
            let Stmt::Assign(a) = s else { continue };
            for t in &a.targets {
                if let Expr::Attribute(at) = t {
                    if let Expr::Name(n) = at.value.as_ref() {
                        if n.id.as_str() == "self" {
                            attrs.insert(at.attr.to_string());
                        }
                    }
                }
            }
        }
    }
    attrs
}

/// A module-level function called from a method with `self.<attr>` arguments
/// (or locals named like the class's attributes) matching EVERY parameter —
/// the class already holds the data, so the function is its method in exile.
pub fn misplaced_method_findings(state: &mut ScanState, body: &[Stmt]) {
    let mut module_fns: std::collections::HashMap<String, (usize, Vec<String>)> = std::collections::HashMap::new();
    for s in body {
        let Stmt::FunctionDef(f) = s else { continue };
        let mut params = Vec::new();
        for a in &f.parameters.posonlyargs {
            params.push(a.parameter.name.to_string());
        }
        for a in &f.parameters.args {
            params.push(a.parameter.name.to_string());
        }
        for a in &f.parameters.kwonlyargs {
            params.push(a.parameter.name.to_string());
        }
        if let Some(v) = &f.parameters.vararg {
            params.push(v.name.to_string());
        }
        if let Some(k) = &f.parameters.kwarg {
            params.push(k.name.to_string());
        }
        let line = line_of(state.source, f.name.range().start());
        module_fns.insert(f.name.to_string(), (line, params));
    }
    if module_fns.is_empty() {
        return;
    }
    for s in body {
        let Stmt::ClassDef(c) = s else { continue };
        let attrs = class_attr_names(&c.body);
        if attrs.is_empty() {
            continue;
        }
        for m in &c.body {
            let Stmt::FunctionDef(method) = m else { continue };
            let mut stack: Vec<&Stmt> = Vec::new();
            for b in &method.body {
                stack.push(b);
            }
            while let Some(st) = stack.pop() {
                if matches!(st, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
                    continue;
                }
                for e in stmt_exprs(st) {
                    let Expr::Call(call) = e else { continue };
                    let Expr::Name(fn_name) = call.func.as_ref() else {
                        continue;
                    };
                    let Some((def_line, params)) = module_fns.get(fn_name.id.as_str()) else {
                        continue;
                    };
                    if params.is_empty() || call.arguments.args.len() < params.len() {
                        continue;
                    }
                    let matched = params.iter().enumerate().all(|(i, p)| {
                        call.arguments.args.get(i).is_some_and(|arg| match arg {
                            Expr::Attribute(a) => {
                                matches!(a.value.as_ref(), Expr::Name(n) if n.id.as_str() == "self")
                                    && a.attr.as_str() == p.as_str()
                            }
                            Expr::Name(n) => n.id.as_str() == p.as_str() && attrs.contains(n.id.as_str()),
                            _ => false,
                        })
                    });
                    if matched {
                        state.findings.push(Finding {
col: 0,
                            file: state.file.to_string(),
                            line: *def_line,
                            function: fn_name.id.to_string(),
                            kind: "misplaced-method".into(),
                            severity: "fail".into(),
                            message: format!(
                                "'{}' is called from '{}.{}' with the class's own state (self.{}) — the class already holds the data; move the function onto the class",
                                fn_name.id.as_str(),
                                c.name.as_str(),
                                method.name.as_str(),
                                params.join(", self.")
                            ),
                        });
                    }
                }
                push_stmt_children(st, &mut stack);
            }
        }
    }
}

/// A dict whose values are same-arity tuples (arity >= 2) — the anonymous
/// record shape. None for mixed/unpacked dicts.
pub fn dict_tuple_arity(e: &Expr) -> Option<usize> {
    match e {
        Expr::Dict(d) => {
            let mut arity = None;
            for item in &d.items {
                item.key.as_ref()?; // **unpacking — cannot verify every value
                match &item.value {
                    Expr::Tuple(t) => {
                        let n = t.elts.len();
                        if n < 2 {
                            return None;
                        }
                        if arity.is_some_and(|a| a != n) {
                            return None;
                        }
                        arity = Some(n);
                    }
                    _ => return None,
                }
            }
            arity
        }
        Expr::DictComp(dc) => match dc.value.as_ref() {
            Expr::Tuple(t) => (t.elts.len() >= 2).then_some(t.elts.len()),
            _ => None,
        },
        _ => None,
    }
}

/// The accessed member of a chain: self.em -> em, cfg.table -> table,
/// em -> em. The closures receiver peel needs the BASE name; the tuple-record
/// reads need the member.
fn attr_chain_last_name(e: &Expr) -> Option<&str> {
    match e {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Attribute(a) => Some(a.attr.as_str()),
        _ => None,
    }
}

/// A literal non-negative integer index (1 in `em[cid][1]`).
fn const_int_index(e: &Expr) -> Option<usize> {
    match e {
        Expr::NumberLiteral(n) => match &n.value {
            ruff_python_ast::Number::Int(i) => i.as_u8().map(|v| v as usize),
            _ => None,
        },
        _ => None,
    }
}

/// Count positional reads of tuple-record dicts: `NAME[k][1]`, `a, b = NAME[k]`,
/// and `for k, (a, b) in NAME.items()` (aliases resolved through module-fn
/// call args).
///
/// Every expression reachable from *e*, INCLUDING comprehension internals
/// (elt/key/value/ifs/iter) — the reads hidden inside comprehensions.
///
/// The ONE expression traversal — every child, including comprehension
/// internals and call-func position (flagged so call-func receivers can be
/// excluded). all_exprs / the receiver-read counter / expr_writes all
/// descend through this; a new Expr variant is handled here once.
fn walk_expr_deep<'a>(e: &'a Expr, in_call_func: bool, f: &mut impl FnMut(&'a Expr, bool)) {
    f(e, in_call_func);
    match e {
        Expr::Call(c) => {
            walk_expr_deep(&c.func, true, f);
            for a in &c.arguments.args {
                walk_expr_deep(a, false, f);
            }
            for k in &c.arguments.keywords {
                walk_expr_deep(&k.value, false, f);
            }
        }
        Expr::Attribute(a) => walk_expr_deep(&a.value, false, f),
        Expr::Subscript(s) => {
            walk_expr_deep(&s.value, false, f);
            walk_expr_deep(&s.slice, false, f);
        }
        Expr::BinOp(b) => {
            walk_expr_deep(&b.left, false, f);
            walk_expr_deep(&b.right, false, f);
        }
        Expr::BoolOp(b) => {
            for v in &b.values {
                walk_expr_deep(v, false, f);
            }
        }
        Expr::Compare(c) => {
            walk_expr_deep(&c.left, false, f);
            for o in &c.comparators {
                walk_expr_deep(o, false, f);
            }
        }
        Expr::UnaryOp(u) => walk_expr_deep(&u.operand, false, f),
        Expr::Lambda(l) => walk_expr_deep(&l.body, false, f),
        Expr::If(i) => {
            walk_expr_deep(&i.test, false, f);
            walk_expr_deep(&i.body, false, f);
            walk_expr_deep(&i.orelse, false, f);
        }
        Expr::Named(n) => walk_expr_deep(&n.value, false, f),
        Expr::Await(a) => walk_expr_deep(&a.value, false, f),
        Expr::Starred(st) => walk_expr_deep(&st.value, false, f),
        Expr::Tuple(t) => {
            for el in &t.elts {
                walk_expr_deep(el, false, f);
            }
        }
        Expr::List(l) => {
            for el in &l.elts {
                walk_expr_deep(el, false, f);
            }
        }
        Expr::Set(st) => {
            for el in &st.elts {
                walk_expr_deep(el, false, f);
            }
        }
        Expr::ListComp(l) => {
            walk_expr_deep(&l.elt, false, f);
            for g in &l.generators {
                walk_expr_deep(&g.iter, false, f);
                for c in &g.ifs {
                    walk_expr_deep(c, false, f);
                }
            }
        }
        Expr::SetComp(sc) => {
            walk_expr_deep(&sc.elt, false, f);
            for g in &sc.generators {
                walk_expr_deep(&g.iter, false, f);
                for c in &g.ifs {
                    walk_expr_deep(c, false, f);
                }
            }
        }
        Expr::DictComp(d) => {
            if let Some(k) = &d.key {
                walk_expr_deep(k, false, f);
            }
            walk_expr_deep(&d.value, false, f);
            for g in &d.generators {
                walk_expr_deep(&g.iter, false, f);
                for c in &g.ifs {
                    walk_expr_deep(c, false, f);
                }
            }
        }
        Expr::Generator(g) => {
            walk_expr_deep(&g.elt, false, f);
            for gg in &g.generators {
                walk_expr_deep(&gg.iter, false, f);
                for c in &gg.ifs {
                    walk_expr_deep(c, false, f);
                }
            }
        }
        _ => {}
    }
}

fn all_exprs<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    walk_expr_deep(e, false, &mut |x, _| out.push(x));
}

pub fn count_tuple_record_reads(
    body: &[Stmt],
    records: &std::collections::HashMap<String, usize>,
    aliases: &std::collections::HashMap<String, String>,
    counts: &mut std::collections::HashMap<String, usize>,
) {
    fn bump(
        base: &str,
        records: &std::collections::HashMap<String, usize>,
        aliases: &std::collections::HashMap<String, String>,
        counts: &mut std::collections::HashMap<String, usize>,
    ) {
        let base = aliases.get(base).map(String::as_str).unwrap_or(base);
        if records.contains_key(base) {
            *counts.entry(base.to_string()).or_insert(0) += 1;
        }
    }
    let mut stack: Vec<&Stmt> = Vec::new();
    for s in body {
        stack.push(s);
    }
    while let Some(s) = stack.pop() {
        if let Stmt::FunctionDef(f) = s {
            for b in &f.body {
                stack.push(b);
            }
            continue; // ClassDef descent is push_stmt_children's job
        }
        // pattern 1: NAME[KEY][K] with a constant index — including the
        // reads nested inside comprehensions
        let mut exprs: Vec<&Expr> = Vec::new();
        for e in stmt_exprs(s) {
            all_exprs(e, &mut exprs);
        }
        for e in exprs {
            if let Expr::Subscript(outer) = e {
                if let Expr::Subscript(inner) = outer.value.as_ref() {
                    if let Some(base) = attr_chain_last_name(&inner.value) {
                        if const_int_index(&outer.slice).is_some() {
                            bump(base, records, aliases, counts);
                        }
                    }
                }
            }
        }
        // pattern 2: `p, nm = NAME[key]` — a tuple of plain names unpacking
        // a record value
        if let Stmt::Assign(a) = s {
            if let Some(Expr::Tuple(t)) = a.targets.first() {
                if t.elts.iter().all(|el| matches!(el, Expr::Name(_))) {
                    if let Expr::Subscript(sub) = a.value.as_ref() {
                        if let Some(b) = attr_chain_last_name(&sub.value) {
                            bump(b, records, aliases, counts);
                        }
                    }
                }
            }
        }
        // pattern 3: `for k, (a, b) in NAME.items()` — a nested tuple target
        if let Stmt::For(f) = s {
            if let Expr::Attribute(attr) = f.iter.as_ref() {
                if attr.attr.as_str() == "items" {
                    if let Some(base) = attr_chain_last_name(&attr.value) {
                        let mut has_nested = false;
                        let mut tstack: Vec<&Expr> = Vec::new();
                        tstack.push(&f.target);
                        while let Some(t) = tstack.pop() {
                            if let Expr::Tuple(tp) = t {
                                if tp.elts.iter().any(|el| matches!(el, Expr::Tuple(_))) {
                                    has_nested = true;
                                }
                                for el in &tp.elts {
                                    tstack.push(el);
                                }
                            }
                        }
                        if has_nested {
                            bump(base, records, aliases, counts);
                        }
                    }
                }
            }
        }
        push_stmt_children(s, &mut stack);
    }
}

/// Fixed element count of a `tuple[...]` annotation — None when the
/// annotation is not a tuple, is unparameterized, or is variadic
/// (`tuple[X, ...]`: a homogeneous sequence, not a record).
fn fixed_tuple_arity(ann: Option<&Expr>) -> Option<usize> {
    let Expr::Subscript(sub) = ann? else {
        return None;
    };
    if !matches!(sub.value.as_ref(), Expr::Name(n) if n.id.as_str() == "tuple") {
        return None;
    }
    let Expr::Tuple(t) = sub.slice.as_ref() else {
        return None;
    };
    if t.elts.iter().any(|e| matches!(e, Expr::EllipsisLiteral(_))) {
        return None;
    }
    let n = t.elts.len();
    (n >= 3).then_some(n)
}

/// A tuple annotation with 3+ fixed elements — a parameter, return, or
/// variable whose positions carry meaning the call site cannot see. The
/// smoosh (packing a long parameter list into one tuple parameter) is THIS
/// finding with fewer lines, not a fix for it: the rule wanted a parameter
/// object with named fields. 2-tuples are idiomatic pairs; 4+ elements
/// fail, 3 warns (the RGB gray zone).
pub fn wide_tuple_findings(state: &mut ScanState, body: &[Stmt]) {
    walk_wide_tuples(state, body, "");
}

/// The annotation walk: every scope, with the enclosing function name for
/// attribution (module-level annotations carry an empty owner).
fn walk_wide_tuples(state: &mut ScanState, body: &[Stmt], owner: &str) {
    for s in body {
        match s {
            Stmt::FunctionDef(f) => {
                for a in f
                    .parameters
                    .posonlyargs
                    .iter()
                    .chain(&f.parameters.args)
                    .chain(&f.parameters.kwonlyargs)
                {
                    emit_wide_tuple(
                        state,
                        a.annotation(),
                        &format!("parameter '{}' of '{}'", a.parameter.name, f.name),
                        line_of(state.source, a.range().start()),
                        f.name.as_str(),
                    );
                }
                emit_wide_tuple(
                    state,
                    f.returns.as_deref(),
                    &format!("the return type of '{}'", f.name),
                    line_of(state.source, f.name.range().start()),
                    f.name.as_str(),
                );
                walk_wide_tuples(state, &f.body, f.name.as_str());
            }
            Stmt::ClassDef(c) => walk_wide_tuples(state, &c.body, owner),
            Stmt::Try(t) => {
                walk_wide_tuples(state, &t.body, owner);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                    walk_wide_tuples(state, &eh.body, owner);
                }
                walk_wide_tuples(state, &t.orelse, owner);
                walk_wide_tuples(state, &t.finalbody, owner);
            }
            Stmt::For(f) => {
                walk_wide_tuples(state, &f.body, owner);
                walk_wide_tuples(state, &f.orelse, owner);
            }
            Stmt::While(w) => {
                walk_wide_tuples(state, &w.body, owner);
                walk_wide_tuples(state, &w.orelse, owner);
            }
            Stmt::If(i) => {
                walk_wide_tuples(state, &i.body, owner);
                for clause in &i.elif_else_clauses {
                    walk_wide_tuples(state, &clause.body, owner);
                }
            }
            Stmt::With(w) => walk_wide_tuples(state, &w.body, owner),
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    emit_wide_tuple(
                        state,
                        Some(a.annotation.as_ref()),
                        &format!("'{}'", n.id.as_str()),
                        line_of(state.source, a.target.range().start()),
                        owner,
                    );
                }
            }
            _ => {}
        }
    }
}

fn emit_wide_tuple(state: &mut ScanState, ann: Option<&Expr>, what: &str, line: usize, function: &str) {
    let Some(n) = fixed_tuple_arity(ann) else {
        return;
    };
    let message = format!(
        "{what} is a {n}-tuple — the positions carry meaning the call site cannot see; introduce a record named with a domain noun, with named fields and a to_dict() at any serialization edge (packing a long parameter list into a tuple is the same defect with fewer lines)"
    );
    // 4+ elements fail; exactly 3 warns (the RGB gray zone). Two pushes so
    // the kind/severity literal pair stays adjacent — `make rules`' drift
    // detector reads the pair from the construction.
    if n >= 4 {
        state.findings.push(Finding {
            col: 0,
            file: state.file.to_string(),
            line,
            function: function.to_string(),
            kind: "wide-tuple".into(),
            severity: "fail".into(),
            message,
        });
    } else {
        state.findings.push(Finding {
            col: 0,
            file: state.file.to_string(),
            line,
            function: function.to_string(),
            kind: "wide-tuple".into(),
            severity: "warn".into(),
            message,
        });
    }
}

/// A dict built with same-arity tuple values, then read with constant
/// integer indexes — an anonymous record; make it a class.
pub fn tuple_record_findings(state: &mut ScanState, body: &[Stmt]) {
    let mut records: std::collections::HashMap<String, (usize, usize)> = std::collections::HashMap::new();
    // build sites at ANY scope (build()'s locals are records too) — walk
    // control flow only, not nested defs (a def owns its own bindings)
    let mut stack: Vec<&Stmt> = Vec::new();
    for s in body {
        stack.push(s);
    }
    while let Some(s) = stack.pop() {
        if let Stmt::FunctionDef(f) = s {
            for b in &f.body {
                stack.push(b);
            }
            continue; // ClassDef descent is push_stmt_children's job
        }
        if let Stmt::Assign(a) = s {
            if a.targets.len() == 1 {
                if let Some(arity) = dict_tuple_arity(&a.value) {
                    if let Expr::Name(n) = &a.targets[0] {
                        let line = line_of(state.source, n.range().start());
                        records.insert(n.id.to_string(), (arity, line));
                    }
                }
            }
        }
        push_stmt_children(s, &mut stack);
    }
    if records.is_empty() {
        return;
    }
    // alias resolution: a module-fn param receiving a record as an argument
    let mut module_params: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for s in body {
        let Stmt::FunctionDef(f) = s else { continue };
        let mut params = Vec::new();
        for a in &f.parameters.args {
            params.push(a.parameter.name.to_string());
        }
        module_params.insert(f.name.to_string(), params);
    }
    let mut aliases: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    {
        let mut stack: Vec<&Stmt> = Vec::new();
        for s in body {
            stack.push(s);
        }
        while let Some(s) = stack.pop() {
            if let Stmt::FunctionDef(f) = s {
                for b in &f.body {
                    stack.push(b);
                }
                continue;
            }
            for e in stmt_exprs(s) {
                let Expr::Call(c) = e else { continue };
                let Expr::Name(fn_name) = c.func.as_ref() else { continue };
                let Some(params) = module_params.get(fn_name.id.as_str()) else {
                    continue;
                };
                for (i, arg) in c.arguments.args.iter().enumerate() {
                    if let (Some(p), Expr::Name(n)) = (params.get(i), arg) {
                        if records.contains_key(n.id.as_str()) {
                            aliases.insert(p.clone(), n.id.to_string());
                        }
                    }
                }
            }
            push_stmt_children(s, &mut stack);
        }
    }
    // count reads across every function body
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let arities: std::collections::HashMap<String, usize> = records.iter().map(|(n, (a, _))| (n.clone(), *a)).collect();
    for s in body {
        match s {
            Stmt::FunctionDef(f) => count_tuple_record_reads(&f.body, &arities, &aliases, &mut counts),
            Stmt::ClassDef(c) => {
                for m in &c.body {
                    if let Stmt::FunctionDef(f) = m {
                        count_tuple_record_reads(&f.body, &arities, &aliases, &mut counts);
                    }
                }
            }
            _ => {}
        }
    }
    for (name, (arity, line)) in &records {
        let reads = counts.get(name).copied().unwrap_or(0);
        if reads >= 3 {
            state.findings.push(Finding {
col: 0,
                file: state.file.to_string(),
                line: *line,
                function: String::new(),
                kind: "tuple-record".into(),
                severity: "fail".into(),
                message: format!(
                    "the values of '{name}' are {arity}-tuples read {reads} times by constant index — an anonymous record; make it a class named with a domain noun (apply: lucidlint fix --kind tuple-record --name <N>) — fix: tuple-record"
                ),
            });
        }
    }
}

/// A function assembles >=3 structures by threading the same data through
/// module-level functions whose outputs feed each other's inputs — a class
/// in waiting: the threaded state belongs on an object.
pub fn assembly_class_findings(state: &mut ScanState, body: &[Stmt]) {
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
            Stmt::FunctionDef(f) => fns.push((f, line_of(state.source, f.name.range().start()))),
            Stmt::ClassDef(c) => {
                for m in &c.body {
                    if let Stmt::FunctionDef(f) = m {
                        fns.push((f, line_of(state.source, f.name.range().start())));
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
        state.findings.push(Finding {
col: 0,
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
            ),
        });
    }
}

// ------------------------------------------------------------- latent-class
// the declaration/ownership half of the family: shared parameter pairs,
// cross-object field reads, and quiet member assignment.

/// One anchor's clump: the functions sharing it and the pairs they share.
#[derive(Default)]
struct AnchorClump {
    functions: Vec<String>,
    pairs: Vec<(String, String)>,
}

/// Three or more module functions sharing the same unordered parameter
/// pair — the pair travels together, so it is a data clump; introduce a
/// parameter object.
pub fn data_clump_findings(state: &mut ScanState, body: &[Stmt]) {
    let mut funcs: Vec<(&StmtFunctionDef, Vec<String>)> = Vec::new();
    for s in body {
        let Stmt::FunctionDef(f) = s else { continue };
        let mut params = Vec::new();
        for a in &f.parameters.posonlyargs {
            params.push(a.parameter.name.to_string());
        }
        for a in &f.parameters.args {
            params.push(a.parameter.name.to_string());
        }
        funcs.push((f, params));
    }
    let mut pairs: std::collections::HashMap<(String, String), Vec<&StmtFunctionDef>> =
        std::collections::HashMap::new();
    for (f, params) in &funcs {
        for i in 0..params.len() {
            for j in (i + 1)..params.len() {
                let (a, b) = if params[i] < params[j] {
                    (params[i].clone(), params[j].clone())
                } else {
                    (params[j].clone(), params[i].clone())
                };
                pairs.entry((a, b)).or_default().push(f);
            }
        }
    }
    // ONE finding per anchor naming every pair that shares it: per-pair
    // findings stack at the same def line and one marker consumes one
    // finding, so an anchor with >3 pairs could never be per-site
    // suppressed — the whack-a-mole the houses sweep hit (suppress the
    // visible one, the next pair surfaces).
    let mut by_anchor: std::collections::HashMap<(usize, String), AnchorClump> = std::collections::HashMap::new();
    let mut reported: HashSet<(String, String)> = HashSet::new();
    for ((a, b), fs) in &pairs {
        if fs.len() < 3 || !reported.insert((a.clone(), b.clone())) {
            continue;
        }
        let line = line_of(state.source, fs[0].name.range().start());
        let entry = by_anchor.entry((line, fs[0].name.to_string())).or_default();
        for f in fs {
            let name = f.name.to_string();
            if !entry.functions.contains(&name) {
                entry.functions.push(name);
            }
        }
        entry.pairs.push((a.clone(), b.clone()));
    }
    let mut anchors: Vec<_> = by_anchor.into_iter().collect();
    anchors.sort_by_key(|((line, _), _)| *line);
    for ((line, anchor), clump) in anchors {
        let (mut names, mut group_pairs) = (clump.functions, clump.pairs);
        names.sort();
        group_pairs.sort();
        let pair_text = group_pairs
            .iter()
            .map(|(a, b)| format!("({a}, {b})"))
            .collect::<Vec<_>>()
            .join(", ");
        let (pair_word, verb) = if group_pairs.len() == 1 {
            ("pair", "travels")
        } else {
            ("pairs", "travel")
        };
        state.findings.push(Finding {
            col: 0,
            file: state.file.to_string(),
            line,
            function: anchor,
            kind: "data-clump".into(),
            severity: "fail".into(),
            message: format!(
                "{} functions ({}) share the parameter {} {} — a data clump: the {} {} together; introduce a parameter object named with a domain noun",
                names.len(),
                names.join(", "),
                pair_word,
                pair_text,
                pair_word,
                verb
            ),
        });
    }
}

/// The root name of an attribute chain (self.graph.em -> self; graph.em ->
/// graph); None for other shapes.
fn chain_root_name(e: &Expr) -> Option<&str> {
    let mut v = e;
    loop {
        match v {
            Expr::Attribute(a) => v = a.value.as_ref(),
            Expr::Name(n) => return Some(n.id.as_str()),
            _ => return None,
        }
    }
}

/// Attribute READ counts by receiver root name — method calls (Call funcs)
/// are not field reads and do not count.
fn count_receiver_reads(body: &[Stmt], counts: &mut std::collections::HashMap<String, usize>) {
    fn visit(e: &Expr, counts: &mut std::collections::HashMap<String, usize>) {
        walk_expr_deep(e, false, &mut |x, in_call_func| {
            if let Expr::Attribute(a) = x {
                if in_call_func {
                    return; // method calls are not field reads
                }
                if let Some(base) = chain_root_name(&a.value) {
                    *counts.entry(base.to_string()).or_insert(0) += 1;
                }
            }
        });
    }

    let mut stack: Vec<&Stmt> = Vec::new();
    for s in body {
        stack.push(s);
    }
    while let Some(s) = stack.pop() {
        if matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            continue;
        }
        for e in stmt_exprs(s) {
            visit(e, counts);
        }
        push_stmt_children(s, &mut stack);
    }
}

/// The locals aliased from the class's own state: `graph = self.graph`.
/// Feature envy reads THESE — the collaborator the method reaches into.
fn collaborator_aliases(body: &[Stmt], out: &mut HashSet<String>) {
    let mut stack: Vec<&Stmt> = Vec::new();
    for s in body {
        stack.push(s);
    }
    while let Some(s) = stack.pop() {
        if matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            continue;
        }
        if let Stmt::Assign(a) = s {
            if a.targets.len() == 1 {
                if let (Expr::Name(n), Expr::Attribute(at)) = (&a.targets[0], a.value.as_ref()) {
                    if matches!(at.value.as_ref(), Expr::Name(inner) if inner.id.as_str() == "self") {
                        out.insert(n.id.to_string());
                    }
                }
            }
        }
        push_stmt_children(s, &mut stack);
    }
}

/// The names bound as loop targets anywhere in a function's body (for x in
/// ...) — elements the function iterates are its own inputs.
fn loop_target_names(body: &[Stmt], out: &mut HashSet<String>) {
    let mut stack: Vec<&Stmt> = Vec::new();
    for s in body {
        stack.push(s);
    }
    while let Some(s) = stack.pop() {
        if matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            continue;
        }
        if let Stmt::For(f) = s {
            let mut tstack: Vec<&Expr> = Vec::new();
            tstack.push(&f.target);
            while let Some(t) = tstack.pop() {
                match t {
                    Expr::Name(n) => {
                        out.insert(n.id.to_string());
                    }
                    Expr::Tuple(tp) => {
                        for el in &tp.elts {
                            tstack.push(el);
                        }
                    }
                    Expr::List(l) => {
                        for el in &l.elts {
                            tstack.push(el);
                        }
                    }
                    Expr::Starred(st) => tstack.push(&st.value),
                    _ => {}
                }
            }
        }
        push_stmt_children(s, &mut stack);
    }
}

/// A method that reads another object's fields more than its own state —
/// feature envy: the computation belongs on the envied object.
pub fn feature_envy_findings(state: &mut ScanState, body: &[Stmt]) {
    for s in body {
        let Stmt::ClassDef(c) = s else { continue };
        for m in &c.body {
            let Stmt::FunctionDef(f) = m else { continue };
            let mut params = HashSet::new();
            function_param_names(&f.parameters, &mut params);
            let mut loop_targets = HashSet::new();
            loop_target_names(&f.body, &mut loop_targets);
            let mut collaborators = HashSet::new();
            collaborator_aliases(&f.body, &mut collaborators);
            let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
            count_receiver_reads(&f.body, &mut counts);
            let self_reads = counts.get("self").copied().unwrap_or(0);
            if self_reads < 1 {
                continue;
            }
            for (receiver, n) in &counts {
                // envy is reaching into a COLLABORATOR (a local aliased from
                // self.<attr>) — not the method's inputs (params, loop
                // targets) and not computed values (pos = self.get_metadata())
                if receiver == "self"
                    || params.contains(receiver)
                    || loop_targets.contains(receiver)
                    || !collaborators.contains(receiver)
                    || *n < 4
                    || *n < self_reads + 3
                {
                    continue;
                }
                let line = line_of(state.source, f.name.range().start());
                state.findings.push(Finding {
col: 0,
                    file: state.file.to_string(),
                    line,
                    function: f.name.to_string(),
                    kind: "feature-envy".into(),
                    severity: "fail".into(),
                    message: format!(
                        "'{}' reads '{}' {} times vs its own state {} — feature envy: the logic belongs on the envied object; move the computation onto '{}' as a method named with a domain verb (what it does) — fix: feature-envy",
                        f.name.as_str(),
                        receiver,
                        n,
                        self_reads,
                        receiver
                    ),
                });
            }
        }
    }
}

/// Collect every `self.<attr> = ...` site in a top-level class — the
/// undeclared-attribute family's raw material. Whether a site is a QUIET
/// assignment is decided at the repo-wide merge (`undeclared_findings`):
/// a base class in another file may declare the member, and a per-file
/// scan cannot see it.
pub fn collect_self_assigns(state: &mut ScanState, body: &[Stmt]) {
    for s in body {
        let Stmt::ClassDef(c) = s else { continue };
        for m in &c.body {
            let Stmt::FunctionDef(f) = m else { continue };
            let is_init = f.name.as_str() == "__init__";
            let mut stack: Vec<&Stmt> = Vec::new();
            for b in &f.body {
                stack.push(b);
            }
            while let Some(st) = stack.pop() {
                if matches!(st, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
                    continue;
                }
                let mut targets: Vec<&Expr> = Vec::new();
                match st {
                    Stmt::Assign(a) => {
                        for t in &a.targets {
                            targets.push(t);
                        }
                    }
                    Stmt::AnnAssign(a) => targets.push(a.target.as_ref()),
                    Stmt::AugAssign(a) => targets.push(&a.target),
                    _ => {}
                }
                for t in targets {
                    let Expr::Attribute(at) = t else { continue };
                    if !matches!(at.value.as_ref(), Expr::Name(n) if n.id.as_str() == "self") {
                        continue;
                    }
                    state.self_assigns.push(crate::SelfAssign {
                        class: c.name.to_string(),
                        attr: at.attr.to_string(),
                        line: line_of(state.source, at.range().start()),
                        function: f.name.to_string(),
                        is_init,
                    });
                }
                push_stmt_children(st, &mut stack);
            }
        }
    }
}

/// The repo-wide undeclared-attribute pass: a `self.<attr> = v` site is a
/// finding only when neither the class NOR ANY ANCESTOR declares the member
/// (annotated in the class body, in __slots__, or in __init__) — the houses
/// case was `Node.display_name` declared in dag/node.py, flagged on every
/// subclass until they re-declared it. Base resolution follows
/// `abstraction_findings`: a `Name:` base resolves same-file first, then
/// through the file's imports; an `Attr:` base through the alias's module.
pub fn undeclared_findings(scans: &[crate::UndeclScan]) -> Vec<Finding> {
    use std::collections::HashMap;
    let mut classes: HashMap<(String, String), &crate::ClassInfo> = HashMap::new();
    for s in scans {
        for c in &s.classes {
            classes.insert((s.rel.clone(), c.name.clone()), c);
        }
    }
    // each class's bases resolve against ITS OWN file's imports
    let import_maps: HashMap<&String, HashMap<&str, (&str, &str)>> = scans
        .iter()
        .map(|s| {
            (
                &s.rel,
                s.imports
                    .iter()
                    .map(|i| (i.alias.as_str(), (i.module.as_str(), i.imported.as_str())))
                    .collect(),
            )
        })
        .collect();
    /// True when `key`'s class or any ANCESTOR declares `attr`; visited
    /// guards against base-class cycles (Python tolerates them at definition
    /// time). A `Name:` base resolves same-file first, then through the
    /// class's own file's imports; an `Attr:` base through the alias's
    /// module. Each hop uses the ancestor's file imports, so multi-level
    /// chains across files resolve correctly.
    fn declares(
        classes: &HashMap<(String, String), &crate::ClassInfo>,
        import_maps: &HashMap<&String, HashMap<&str, (&str, &str)>>,
        key: &(String, String),
        attr: &str,
        visited: &mut Vec<(String, String)>,
    ) -> bool {
        if visited.contains(key) {
            return false;
        }
        visited.push(key.clone());
        let Some(c) = classes.get(key) else {
            return false;
        };
        if c.declared.iter().any(|d| d == attr) {
            return true;
        }
        let empty = HashMap::new();
        let imports_of = import_maps.get(&key.0).unwrap_or(&empty);
        for base in &c.bases {
            let candidates: Vec<(String, String)> = if let Some(rest) = base.strip_prefix("Name:") {
                let mut cands = vec![(key.0.clone(), rest.to_string())];
                if let Some((module, _name)) = imports_of.get(rest) {
                    cands.push(((*module).to_string(), rest.to_string()));
                }
                cands
            } else if let Some(rest) = base.strip_prefix("Attr:") {
                let mut parts = rest.splitn(2, ':');
                let alias = parts.next().unwrap_or("");
                let attr_name = parts.next().unwrap_or("");
                let mut cands = Vec::new();
                if let Some((module, _name)) = imports_of.get(alias) {
                    cands.push(((*module).to_string(), attr_name.to_string()));
                }
                cands
            } else {
                Vec::new()
            };
            for anc in candidates {
                let Some(anc_key) = class_key(classes, &anc.0, &anc.1) else {
                    continue;
                };
                if declares(classes, import_maps, &anc_key, attr, visited) {
                    return true;
                }
            }
        }
        false
    }
    let mut out = Vec::new();
    for s in scans {
        for a in &s.self_assigns {
            let mut visited: Vec<(String, String)> = Vec::new();
            let key = (s.rel.clone(), a.class.clone());
            if declares(&classes, &import_maps, &key, &a.attr, &mut visited) {
                continue;
            }
            let msg = if a.is_init {
                format!(
                    "'{}' assigns member '{}' in __init__ without a declaration — declare it: self.{}: <type> = ... — fix: undeclared-attribute",
                    a.class, a.attr, a.attr
                )
            } else {
                format!(
                    "'{}' assigns member '{}' in '{}' without a declaration — declare it in __init__ (annotated) or the class body",
                    a.class, a.attr, a.function
                )
            };
            out.push(Finding {
                col: 0,
                file: s.rel.clone(),
                line: a.line,
                function: a.function.clone(),
                kind: "undeclared-attribute".into(),
                severity: "fail".into(),
                message: msg,
            });
        }
    }
    out.sort_by(|x, y| (&x.file, x.line, &x.message).cmp(&(&y.file, y.line, &y.message)));
    out
}

/// The names in a __slots__ literal (tuple or list of string literals).
fn slot_names(e: &Expr) -> Option<Vec<String>> {
    let elts: Vec<&Expr> = match e {
        Expr::Tuple(t) => t.elts.iter().collect(),
        Expr::List(l) => l.elts.iter().collect(),
        _ => return None,
    };
    let mut names = Vec::new();
    for el in elts {
        if let Expr::StringLiteral(s) = el {
            names.push(s.value.to_string());
        } else {
            return None;
        }
    }
    Some(names)
}

// ------------------------------------------------------------- latent-class
// the size + duplication counterweights: a class too large to review, and
// domain state duplicated across a containment edge.

/// A class so large it strains review: 20 or more methods, or 12 or more
/// methods over a 250-line span. A WARN — the split is only sound where the
/// partition rule finds field-disjoint method groups; size alone never
/// forces a bad split.
pub fn god_class_findings(state: &mut ScanState, body: &[Stmt]) {
    for s in body {
        let Stmt::ClassDef(c) = s else { continue };
        let methods = c.body.iter().filter(|m| matches!(m, Stmt::FunctionDef(_))).count();
        if methods < 12 {
            continue;
        }
        let span = line_of(state.source, c.range().end()).saturating_sub(line_of(state.source, c.range().start()));
        let big = methods >= 20 || (methods >= 12 && span >= 250);
        if !big {
            continue;
        }
        let line = line_of(state.source, c.name.range().start());
        state.findings.push(Finding {
col: 0,
            file: state.file.to_string(),
            line,
            function: c.name.to_string(),
            kind: "god-class".into(),
            severity: "warn".into(),
            message: format!(
                "'{}' has {methods} methods over {span} lines — a large class; split it ONLY where the partition rule finds field-disjoint method groups (size alone is a review signal, not a split order)",
                c.name.as_str()
            ),
        });
    }
}

/// A class's field names (class-level annotated declarations + self.X
/// assignments in __init__), with the annotation type where present.
fn class_field_names(c: &StmtClassDef) -> Vec<(String, Option<String>)> {
    let mut fields: Vec<(String, Option<String>)> = Vec::new();
    for m in &c.body {
        match m {
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    let ty = annotation_base_name(a.annotation.as_ref()).map(|s| s.to_string());
                    fields.push((n.id.to_string(), ty));
                }
            }
            Stmt::FunctionDef(f) => {
                if f.name.as_str() != "__init__" {
                    continue;
                }
                for st in &f.body {
                    if let Stmt::AnnAssign(a) = st {
                        if let Expr::Attribute(at) = a.target.as_ref() {
                            if matches!(at.value.as_ref(), Expr::Name(n) if n.id.as_str() == "self") {
                                let ty = annotation_base_name(a.annotation.as_ref()).map(|s| s.to_string());
                                fields.push((at.attr.to_string(), ty));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    fields
}

/// The same domain state on a class and on a class it CONTAINS — the
/// duplicate lives across a containment edge; one source of truth should
/// own it.
pub fn duplicate_field_findings(state: &mut ScanState, body: &[Stmt]) {
    let classes: Vec<&StmtClassDef> = body
        .iter()
        .filter_map(|s| match s {
            Stmt::ClassDef(c) => Some(c),
            _ => None,
        })
        .collect();
    for c in &classes {
        let fields = class_field_names(c);
        for ty in fields.iter().map(|(_, t)| t) {
            let Some(ty) = ty else { continue };
            let Some(contained) = classes.iter().find(|b| b.name.as_str() == ty.as_str()) else {
                continue;
            };
            if std::ptr::eq(*c, *contained) {
                continue;
            }
            // the shared field set across the containment edge — >=2 shared
            // names is duplicated domain state (one generic name like `name`
            // is coincidence)
            let contained_fields = class_field_names(contained);
            let shared: Vec<&str> = fields
                .iter()
                .filter(|(n, _)| contained_fields.iter().any(|(cn, _)| cn == n))
                .map(|(n, _)| n.as_str())
                .collect();
            if shared.len() < 2 {
                continue;
            }
            let line = line_of(state.source, c.name.range().start());
            state.findings.push(Finding {
col: 0,
                file: state.file.to_string(),
                line,
                function: c.name.to_string(),
                kind: "duplicate-field".into(),
                severity: "fail".into(),
                message: format!(
                    "'{}' and the '{}' it contains both hold {} — the same data lives in two places; one of them should own it",
                    c.name.as_str(),
                    contained.name.as_str(),
                    shared.join(", ")
                ),
            });
        }
    }
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
/// file stem — the class-module rule. A module that ALSO defines module-level
/// functions is a tool script, not a class file (the class is a component,
/// not the module's identity) — renaming it after the class would be wrong.
pub fn class_module_findings(state: &mut ScanState, module_body: &[Stmt], rel: &str) {
    if rel.ends_with("__init__.py") {
        return;
    }
    let classes: Vec<&Stmt> = module_body.iter().filter(|s| matches!(s, Stmt::ClassDef(_))).collect();
    if classes.len() != 1 {
        return;
    }
    let has_module_fns = module_body.iter().any(|s| matches!(s, Stmt::FunctionDef(_)));
    if has_module_fns {
        return;
    }
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
        col: 0,
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
col: 0,
                file: state.file.to_string(),
                line,
                function: cls.name.to_string(),
                kind: "vague-name".into(),
                severity: "fail".into(),
                message: format!(
                    "'{suffix}' name carries a {span}-line class with {methods} methods — the domain noun should take the name — fix: vague-name --fix-name <Name>"
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
            // strewing is "they SHARE DATA": a fn that never reads its leading
            // param has nothing to share — moving it into the class would make
            // the class field-disjoint (partition) and the fix would thrash
            let recv = f.parameters.args[0].parameter.name.id.as_str();
            if class_names.contains(&base) && body_refs_name(&f.body, recv) {
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
col: 0,
            file: state.file.to_string(),
            line,
            function: String::new(),
            kind: "strewing".into(),
            severity: "fail".into(),
            message: format!(
                "{} free functions all take '{base}' as their first argument — a '{base}' class is missing, and these functions are its methods: {} — fix: extract-class",
                members.len(),
                names.join(", ")
            ),
        });
    }
}

// ---------------------------------------------------------------------------
// review-log rules (family-album log §10/§11): duplicate module-scope
// definitions, restating docstrings, duplicated statement blocks — all
// deterministic on the AST the scan already walks.
// ---------------------------------------------------------------------------

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
        // (bound name, is the binder a def/class?) — only a DEF/CLASS shadow
        // is a hazard worth flagging: `import urllib` + `import urllib.request`
        // both bind `urllib` idiomatically, and the §10 def-in-imports
        // mistake is exactly a def landing on an import's name.
        let mut bindings: Vec<(String, bool, bool)> = Vec::new(); // (name, is def, overload stub)
        match s {
            Stmt::FunctionDef(f) => {
                let stub = f.decorator_list.iter().any(|d| {
                    matches!(&d.expression, Expr::Name(n) if overload_bound.contains(n.id.as_str()))
                        || matches!(&d.expression, Expr::Attribute(a) if a.attr.as_str() == "overload")
                });
                bindings.push((f.name.to_string(), true, stub));
            }
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
        for (name, is_def, stub) in bindings {
            if name.is_empty() {
                continue;
            }
            if is_def {
                let exempt = stub || (overloaded.contains(&name) && impl_def_line.get(&name).copied() == Some(line));
                if !exempt {
                    if let Some((_, first_line)) = seen.iter().find(|(n, _)| *n == name) {
                        state.findings.push(Finding {
col: 0,
                            file: state.file.to_string(),
                            line,
                            function: name.clone(),
                            kind: "duplicate-def".into(),
                            severity: "fail".into(),
                            message: format!(
                                "module-scope name '{name}' is bound twice (first at line {first_line}) — the later definition shadows the earlier: a shadowing hazard (dispatch or edit mistake); rename one — fix: duplicate-def"
                            ),
                        });
                    }
                }
            }
            seen.push((name, line));
        }
    }
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

/// Docstring content words that add nothing the body does not already say
/// (review-log §11.5: "the heaviest comment load restated the body"). A
/// docstring whose content words all appear in the body's own tokens is
/// comment noise — name the concept instead.
const DOCSTRING_STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "in", "for", "on", "with", "its", "it", "is", "are", "be", "was",
    "were", "this", "that", "these", "those", "from", "as", "at", "by", "into", "over", "under", "when", "while", "if",
    "not", "no", "any", "all", "each", "their", "his", "her", "we", "you", "they",
    // modals are prose structure, never code: "must be consistent with" —
    // the content words are the identifiers the body already names
    "must", "will", "should", "would", "can", "could", "may", "might", "shall",
];

fn docstring_content_words(doc: &str) -> Vec<String> {
    doc.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3 && !DOCSTRING_STOPWORDS.contains(w))
        .map(|w| w.to_lowercase())
        .collect()
}

/// The body's identifier/string tokens — what a docstring word must cover to
/// count as "already said".
fn body_tokens(body: &[Stmt]) -> HashSet<String> {
    use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
    struct Tokens(HashSet<String>);
    impl<'a> SourceOrderVisitor<'a> for Tokens {
        fn visit_expr(&mut self, e: &'a Expr) {
            match e {
                Expr::Name(n) => {
                    self.0.insert(n.id.to_lowercase());
                }
                Expr::Attribute(a) => {
                    self.0.insert(a.attr.to_lowercase());
                }
                Expr::StringLiteral(l) => {
                    self.0.insert(l.value.to_str().to_lowercase());
                }
                _ => {}
            }
            walk_stmt_tokens(self, e);
        }
        fn visit_keyword(&mut self, k: &'a ruff_python_ast::Keyword) {
            if let Some(arg) = &k.arg {
                self.0.insert(arg.to_lowercase());
            }
            ruff_python_ast::visitor::source_order::walk_keyword(self, k);
        }
    }
    fn walk_stmt_tokens<'a, V: SourceOrderVisitor<'a>>(v: &mut V, e: &'a Expr) {
        ruff_python_ast::visitor::source_order::walk_expr(v, e);
    }
    let mut t = Tokens(HashSet::new());
    for s in body {
        walk_stmt(&mut t, s);
    }
    t.0
}

/// A module-scope def/class whose docstring restates its own body (warn).
pub fn restating_docstring_findings(state: &mut ScanState, module_body: &[Stmt]) {
    for s in module_body {
        let body: &[Stmt] = match s {
            Stmt::FunctionDef(f) => &f.body,
            Stmt::ClassDef(c) => &c.body,
            _ => continue,
        };
        let Some(Stmt::Expr(e)) = body.first() else { continue };
        let Expr::StringLiteral(lit) = e.value.as_ref() else {
            continue;
        };
        let doc = lit.value.to_str();
        let words = docstring_content_words(doc);
        if words.len() < 5 {
            continue; // short docstrings carry the name; not noise
        }
        let tokens = body_tokens(&body[1..]);
        let covered = words.iter().filter(|w| tokens.contains(w.as_str())).count();
        if covered as f64 / words.len() as f64 >= 0.9 {
            let line = stmt_line(state.source, s);
            state.findings.push(Finding {
col: 0,
                file: state.file.to_string(),
                line,
                function: String::new(),
                kind: "restating-docstring".into(),
                severity: "warn".into(),
                message: format!(
                    "docstring restates the body — {covered}/{} content words appear in the code; name the concept instead — fix: restating-docstring",
                    words.len()
                ),
            });
        }
    }
}

/// An identical statement block (>= 3 statements) appearing twice in one
/// function body — the edit-mistake signature (a replaced loop header leaves
/// the old body in place; the review-log fingerprint work's transcribe-twice
/// class, where every page would be processed and billed twice). warn:
/// repeated identical blocks are provably duplicated work.
const BLOCK_WINDOW: usize = 3;

fn flatten_stmts<'a>(stmts: &'a [Stmt], out: &mut Vec<&'a Stmt>) {
    for s in stmts {
        out.push(s);
        match s {
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {}
            Stmt::If(i) => {
                flatten_stmts(&i.body, out);
                for cl in &i.elif_else_clauses {
                    flatten_stmts(&cl.body, out);
                }
            }
            Stmt::While(w) => {
                flatten_stmts(&w.body, out);
                flatten_stmts(&w.orelse, out);
            }
            Stmt::For(fr) => {
                flatten_stmts(&fr.body, out);
                flatten_stmts(&fr.orelse, out);
            }
            Stmt::With(w) => flatten_stmts(&w.body, out),
            Stmt::Try(t) => {
                flatten_stmts(&t.body, out);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                    flatten_stmts(&eh.body, out);
                }
                flatten_stmts(&t.orelse, out);
                flatten_stmts(&t.finalbody, out);
            }
            Stmt::Match(m) => {
                for case in &m.cases {
                    flatten_stmts(&case.body, out);
                }
            }
            _ => {}
        }
    }
}

pub fn duplicate_block_findings(state: &mut ScanState, module_body: &[Stmt]) {
    use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
    struct Bodies<'a>(Vec<&'a [Stmt]>);
    impl<'a> SourceOrderVisitor<'a> for Bodies<'a> {
        fn visit_stmt(&mut self, s: &'a Stmt) {
            if let Stmt::FunctionDef(f) = s {
                self.0.push(&f.body);
            }
            walk_stmt(self, s);
        }
    }
    let mut b = Bodies(Vec::new());
    for s in module_body {
        // walk_stmt visits DESCENDANTS only — the top-level def is invisible
        // to visit_stmt, so collect it explicitly (nested defs fire via the
        // descent)
        if let Stmt::FunctionDef(f) = s {
            b.0.push(&f.body);
        }
        walk_stmt(&mut b, s);
    }
    for body in &b.0 {
        let mut flat: Vec<&Stmt> = Vec::new();
        flatten_stmts(body, &mut flat);
        if flat.len() < BLOCK_WINDOW * 2 {
            continue;
        }
        // node-index-free structural keys: Stmt's PartialEq includes the
        // parser's AtomicNodeIndex, so two identical statements are never
        // `==` — the skeleton key is the comparison. First-seen map: a
        // window keyed by its joined statements finds its duplicate in O(1)
        // (the naive i/j scan is O(n^2) and a generated 15k-statement
        // function pays real seconds for it).
        let keys: Vec<Vec<String>> = flat.iter().map(|s| stmt_key(s)).collect();
        let mut first_seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        'outer: for j in 0..=keys.len() - BLOCK_WINDOW {
            let mut window = String::new();
            for k in 0..BLOCK_WINDOW {
                for tok in &keys[j + k] {
                    window.push_str(tok);
                    window.push('\u{1}');
                }
            }
            if let Some(&i) = first_seen.get(&window) {
                if i + BLOCK_WINDOW <= j {
                    let line = stmt_line(state.source, flat[j]);
                    // the directive is truthful only for deep-equal blocks
                    // (raw keys match); renamed duplicates keep the finding
                    // but no fix — agents delete by hand (review bot)
                    let exact = (0..BLOCK_WINDOW).all(|k| stmt_exact_key(flat[i + k]) == stmt_exact_key(flat[j + k]));
                    let directive = if exact { " — fix: duplicate-block" } else { "" };
                    state.findings.push(Finding {
col: 0,
                        file: state.file.to_string(),
                        line,
                        function: String::new(),
                        kind: "duplicate-block".into(),
                        severity: "warn".into(),
                        message: format!(
                            "a {BLOCK_WINDOW}-statement block appears twice in this function — duplicated work (an edit mistake?); delete the second copy{directive}"
                        ),
                    });
                    break 'outer;
                }
            } else {
                first_seen.insert(window, j);
            }
        }
    }
}

/// A node-index-free structural key for ONE statement — the same BFS the
/// duplicate skeleton uses, seeded with the statement. Two statements are
/// "identical" for the duplicate-block rule when their keys are equal.
fn stmt_key(s: &Stmt) -> Vec<String> {
    let mut toks: Vec<String> = Vec::new();
    let mut queue: Vec<Q> = vec![Q::N(AnyNodeRef::from(s))];
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
/// The RAW token key — same walk as `stmt_key` but without the name/literal
/// normalization: two blocks are deep-equal (the libcst `deep_equals` the
/// duplicate-block fix requires) only when their raw keys match. The
/// duplicate-block FINDING fires on normalized keys (renamed transcription
/// duplicates are the rule's target) but the fix DIRECTIVE must not attach
/// to a block the fix will refuse (review bot).
fn stmt_exact_key(s: &Stmt) -> Vec<String> {
    let mut toks: Vec<String> = Vec::new();
    let mut queue: Vec<Q> = vec![Q::N(AnyNodeRef::from(s))];
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
            AnyNodeRef::ExprName(n) => toks.push(n.id.to_string()),
            AnyNodeRef::ExprStringLiteral(_)
            | AnyNodeRef::ExprBytesLiteral(_)
            | AnyNodeRef::ExprNumberLiteral(_)
            | AnyNodeRef::ExprBooleanLiteral(_)
            | AnyNodeRef::ExprNoneLiteral(_)
            | AnyNodeRef::ExprEllipsisLiteral(_)
            | AnyNodeRef::InterpolatedStringLiteralElement(_) => toks.push(format!("{:?}", node.kind())),
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
                col: 0,
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

/// Offsets of numeric literals that are data-table entries, exempt from
/// magic-number: inside a single collection literal (dict/list/set/tuple,
/// nested collections allowed) where at least 3 same-kind numeric siblings
/// (same int/float type, same operator) sit directly under a BinOp/UnaryOp/
/// Compare. The literal IS the data there — `{"A": 6/9, "B": 7/9, ...}` and
/// `{"UB": (51.4, 51.6, -0.5, 0.0), ...}` — naming each would destroy the
/// table. A number is exempt when ANY enclosing collection's sibling group
/// reaches the bar (the geo case only clears at the dict level, not per row).
pub fn magic_table_exempt_offsets(body: &[Stmt]) -> HashSet<usize> {
    use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
    struct CollectionFinder<'a> {
        collections: Vec<&'a Expr>,
    }
    impl<'a> SourceOrderVisitor<'a> for CollectionFinder<'a> {
        fn visit_expr(&mut self, e: &'a Expr) {
            if matches!(e, Expr::Dict(_) | Expr::List(_) | Expr::Set(_) | Expr::Tuple(_)) {
                self.collections.push(e);
            }
            walk_expr(self, e);
        }
    }
    fn walk_expr<'a, V: SourceOrderVisitor<'a>>(v: &mut V, e: &'a Expr) {
        ruff_python_ast::visitor::source_order::walk_expr(v, e);
    }
    let mut finder = CollectionFinder {
        collections: Vec::new(),
    };
    for s in body {
        walk_stmt(&mut finder, s);
    }
    let mut exempt: HashSet<usize> = HashSet::new();
    for c in finder.collections {
        let mut groups: std::collections::HashMap<(bool, String), Vec<usize>> = std::collections::HashMap::new();
        census_collection(c, &mut groups);
        for offsets in groups.values() {
            if offsets.len() >= 3 {
                exempt.extend(offsets.iter().copied());
            }
        }
    }
    exempt
}

/// Census a collection literal's subtree: numbers whose op-parent chain up to
/// this collection passes only through collection literals.
fn census_collection(e: &Expr, groups: &mut std::collections::HashMap<(bool, String), Vec<usize>>) {
    match e {
        Expr::Dict(d) => {
            for item in &d.items {
                if let Some(k) = &item.key {
                    census_value(k, groups);
                }
                census_value(&item.value, groups);
            }
        }
        Expr::List(l) => {
            for el in &l.elts {
                census_value(el, groups);
            }
        }
        Expr::Set(s) => {
            for el in &s.elts {
                census_value(el, groups);
            }
        }
        Expr::Tuple(t) => {
            for el in &t.elts {
                census_value(el, groups);
            }
        }
        _ => {}
    }
}

/// A direct element of a collection: nested collections recurse; an op's
/// direct number children are data candidates; anything else (calls,
/// subscripts, comprehensions) stops the census — not table data.
fn census_value(e: &Expr, groups: &mut std::collections::HashMap<(bool, String), Vec<usize>>) {
    match e {
        Expr::Dict(_) | Expr::List(_) | Expr::Set(_) | Expr::Tuple(_) => census_collection(e, groups),
        Expr::BinOp(b) => {
            let op = format!("{:?}", b.op);
            census_op_number(&b.left, &op, groups);
            census_op_number(&b.right, &op, groups);
        }
        Expr::UnaryOp(u) => {
            let op = format!("{:?}", u.op);
            census_op_number(&u.operand, &op, groups);
        }
        Expr::Compare(c) => {
            let op = format!("{:?}", c.ops);
            census_op_number(&c.left, &op, groups);
            for cmp in &c.comparators {
                census_op_number(cmp, &op, groups);
            }
        }
        _ => {}
    }
}

fn census_op_number(e: &Expr, op: &str, groups: &mut std::collections::HashMap<(bool, String), Vec<usize>>) {
    if let Expr::NumberLiteral(n) = e {
        let is_int = matches!(n.value, ruff_python_ast::Number::Int(_));
        let value = match &n.value {
            ruff_python_ast::Number::Int(i) => i.to_string(),
            ruff_python_ast::Number::Float(f) => f.to_string(),
            ruff_python_ast::Number::Complex { .. } => return,
        };
        if matches!(value.as_str(), "0" | "1" | "2") {
            return; // never magic anyway
        }
        groups
            .entry((is_int, op.to_string()))
            .or_default()
            .push(n.range().start().to_usize());
    }
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
// advisory refactorings (warn): the detection half of Fowler refactorings
// whose fixes are too involved to auto-apply. Each message names the
// refactoring so the agent can hand-apply it (or a future fix can).

/// Guard Clauses: a chain of if-in-if ("arrow code") — the body of an if is
/// exactly another if. Depth >= 3 levels. The fix (invert to early returns)
/// is control-flow surgery; the finding names the refactoring.
pub fn guard_clause_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    fn chain_len(s: &Stmt) -> usize {
        if let Stmt::If(i) = s {
            if i.body.len() == 1 && i.elif_else_clauses.is_empty() && matches!(i.body[0], Stmt::If(_)) {
                return 1 + chain_len(&i.body[0]);
            }
        }
        1
    }
    fn walk(state: &mut ScanState, stmts: &[Stmt], source: &str) {
        for s in stmts {
            if let Stmt::If(i) = s {
                let len = chain_len(s);
                if len >= 3 {
                    let line = line_of(source, s.range().start());
                    state.findings.push(Finding {
col: 0,
                        file: state.file.to_string(),
                        line,
                        function: state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default(),
                        kind: "guard-clauses".into(),
                        severity: "warn".into(),
                        message: format!(
                            "{len} levels of nested if — Replace Nested Conditional with Guard Clauses: invert the conditions to early returns"
                        ),
                    });
                }
                // descend: nested ifs inside the body (not the chain itself)
                if !(i.body.len() == 1 && matches!(i.body[0], Stmt::If(_)) && i.elif_else_clauses.is_empty()) {
                    walk(state, &i.body, source);
                }
                for cl in &i.elif_else_clauses {
                    walk(state, &cl.body, source);
                }
            } else {
                walk_children(state, s, source);
            }
        }
    }
    fn walk_children(state: &mut ScanState, s: &Stmt, source: &str) {
        // descend into any nested statement containers (function/class/loop bodies)
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
            Stmt::With(w) => walk(state, &w.body, source),
            _ => {}
        }
    }
    walk(state, body, source);
}

/// The dispatch-key (left side) of a chain-arm test — the value all arms
/// compare. None when the test is not a simple comparison on one value.
fn dispatch_key(e: &Expr) -> Option<String> {
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

fn base_name_of(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => base_name_of(&a.value),
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
                        state.findings.push(Finding {
col: 0,
                            file: state.file.to_string(),
                            line,
                            function: state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default(),
                            kind: "conditional-polymorphism".into(),
                            severity: "warn".into(),
                            message: format!(
                                "{} arms dispatch on the same value ('{}') — Replace Conditional with Polymorphism: one method per case",
                                arms.len(),
                                keys[0].as_deref().unwrap_or("")
                            ),
                        });
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

/// Introduce Special Case: >= 3 None/empty checks on the same name — the
/// repeated null-handling the special-case object replaces.
pub fn special_case_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let mut counts: std::collections::HashMap<String, (usize, usize)> = std::collections::HashMap::new();
    scan_stmts(body, &mut counts, source);

    let mut v: Vec<(&String, &(usize, usize))> = counts.iter().collect();
    v.sort_by_key(|(_, (_, l))| *l);
    for (name, (n, line)) in v {
        if *n >= 3 {
            state.findings.push(Finding {
col: 0,
                file: state.file.to_string(),
                line: *line,
                function: String::new(),
                kind: "special-case".into(),
                severity: "warn".into(),
                message: format!(
                    "{n} repeated None/empty checks on '{name}' — Introduce Special Case: handle the missing or empty value once (a default or a stand-in object) instead of checking it at every use"
                ),
            });
        }
    }
}

/// Remove Middle Man: a method whose body is a single delegation call
/// (`return self.x.y(...)` / `return self.y(...)`) — it only forwards.
pub fn middle_man_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    fn is_self_call(e: &Expr) -> bool {
        match e {
            Expr::Call(c) => match c.func.as_ref() {
                Expr::Attribute(a) => base_name_of(&a.value).as_deref() == Some("self"),
                _ => false,
            },
            _ => false,
        }
    }
    fn walk(state: &mut ScanState, stmts: &[Stmt], source: &str) {
        for s in stmts {
            match s {
                Stmt::FunctionDef(f) => {
                    if f.body.len() == 1 {
                        if let Stmt::Return(r) = &f.body[0] {
                            if let Some(v) = &r.value {
                                if is_self_call(v) {
                                    let line = line_of(source, f.name.range().start());
                                    state.findings.push(Finding {
col: 0,
                                        file: state.file.to_string(),
                                        line,
                                        function: f.name.to_string(),
                                        kind: "middle-man".into(),
                                        severity: "warn".into(),
                                        message: format!(
                                            "'{}' is a pure delegation to self — Remove Middle Man: call the target directly",
                                            f.name
                                        ),
                                    });
                                }
                            }
                        }
                    }
                }
                Stmt::ClassDef(cd) => walk(state, &cd.body, source),
                Stmt::For(f) => walk(state, &f.body, source),
                Stmt::While(w) => walk(state, &w.body, source),
                Stmt::If(i) => {
                    walk(state, &i.body, source);
                    for cl in &i.elif_else_clauses {
                        walk(state, &cl.body, source);
                    }
                }
                _ => {}
            }
        }
    }
    walk(state, body, source);
}

/// Collect `set_*` methods (and property setters) for the repo-wide
/// unused-setter pass — runs per file; reference counting needs the whole
/// repo (prod vs test split) to distinguish dead from test-only.
pub fn collect_setters(state: &mut ScanState, body: &[Stmt]) {
    fn walk(state: &mut ScanState, stmts: &[Stmt], source: &str) {
        for s in stmts {
            match s {
                Stmt::FunctionDef(f) => {
                    let is_setter = f.name.as_str().starts_with("set_")
                        || f.decorator_list
                            .iter()
                            .any(|d| matches!(&d.expression, Expr::Attribute(a) if a.attr.as_str() == "setter"));
                    if is_setter {
                        state
                            .setters
                            .push((f.name.to_string(), line_of(source, f.name.range().start())));
                    }
                }
                Stmt::ClassDef(cd) => walk(state, &cd.body, source),
                _ => {}
            }
        }
    }
    walk(state, body, state.source);
}

/// Remove Setting Method: a `set_*` method (or a property setter) referenced
/// nowhere in production is dead — deletable, not just unused. Runs
/// repo-wide so prod refs (same-file AND cross-file) are complete.
///
/// Test-only references do NOT make it live: a test seam for code nothing
/// ships calls is not a seam, it is dead code wearing a harness. The message
/// names the imminent-caller escape so an agent mid-refactor is not wrongly
/// told to delete.
pub fn unused_setter_findings(
    setters: &[(String, String, usize)],
    prod_refs: &HashSet<String>,
    test_refs: &HashSet<String>,
) -> Vec<Finding> {
    let mut out = Vec::new();
    for (rel, name, line) in setters {
        if prod_refs.contains(name) {
            continue; // referenced by production — live
        }
        let test_only = test_refs.contains(name);
        let message = if test_only {
            format!(
                "setter '{name}' ({rel}:{line}) is referenced only from tests, never from production — a test seam for dead code is not a seam: nothing shipped calls it, so it is dead. If the production caller is imminent, write the calling code — this finding then clears. Public API entry points are exempt"
            )
        } else {
            format!("setter '{name}' ({rel}:{line}) is never referenced — Remove Setting Method: delete it")
        };
        out.push(Finding {
            file: rel.to_string(),
            line: *line,
            col: 0,
            function: name.to_string(),
            kind: "unused-setter".into(),
            severity: "warn".into(),
            message,
        });
    }
    out
}

/// Replace Loop with Pipeline: a for-loop whose body is only a collection
/// mutation (append/add/update or a subscript store) with at most one
/// if-filter — the shape a comprehension replaces.
pub fn loop_pipeline_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    fn is_collection_mutation(e: &Expr) -> bool {
        match e {
            Expr::Call(c) => matches!(c.func.as_ref(), Expr::Attribute(a)
                if matches!(a.attr.as_str(), "append" | "add" | "extend" | "update" | "appendleft" | "add_update")),
            Expr::Subscript(_) => true, // result[k] = v
            _ => false,
        }
    }
    fn body_is_pipeline(stmts: &[Stmt]) -> bool {
        match stmts {
            [Stmt::Expr(e)] => is_collection_mutation(&e.value),
            [Stmt::If(i), ..] => {
                i.body.len() == 1 && matches!(&i.body[0], Stmt::Expr(e) if is_collection_mutation(&e.value))
            }
            _ => false,
        }
    }
    fn walk(state: &mut ScanState, stmts: &[Stmt], source: &str) {
        for s in stmts {
            match s {
                Stmt::For(f) => {
                    if body_is_pipeline(&f.body) {
                        let line = line_of(source, f.range().start());
                        state.findings.push(Finding {
                            col: 0,
                            file: state.file.to_string(),
                            line,
                            function: state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default(),
                            kind: "loop-pipeline".into(),
                            severity: "warn".into(),
                            message: "loop builds a collection — Replace Loop with Pipeline: use a comprehension"
                                .into(),
                        });
                    }
                    walk(state, &f.body, source);
                }
                Stmt::ClassDef(cd) => walk(state, &cd.body, source),
                Stmt::FunctionDef(f) => walk(state, &f.body, source),
                Stmt::While(w) => walk(state, &w.body, source),
                Stmt::If(i) => {
                    walk(state, &i.body, source);
                    for cl in &i.elif_else_clauses {
                        walk(state, &cl.body, source);
                    }
                }
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
    walk(state, body, source);
}

fn is_empty_literal(e: &Expr) -> bool {
    matches!(e, Expr::StringLiteral(s) if s.value.is_empty())
        || matches!(e, Expr::List(l) if l.elts.is_empty())
        || matches!(e, Expr::Dict(d) if d.items.is_empty())
        || matches!(e, Expr::Tuple(t) if t.elts.is_empty())
}
fn scan_stmts(stmts: &[Stmt], counts: &mut std::collections::HashMap<String, (usize, usize)>, source: &str) {
    for s in stmts {
        if let Stmt::If(i) = s {
            if let Expr::Compare(c) = i.test.as_ref() {
                if let Expr::Name(n) = c.left.as_ref() {
                    let none_check = c.ops.len() == 1
                        && matches!(c.ops[0], ruff_python_ast::CmpOp::Is)
                        && matches!(c.comparators[0], Expr::NoneLiteral(_));
                    let empty_check = c.ops.len() == 1
                        && matches!(c.ops[0], ruff_python_ast::CmpOp::Eq)
                        && is_empty_literal(&c.comparators[0]);
                    if none_check || empty_check {
                        // a fail-fast GUARD (the guarded branch raises) is not
                        // a special-case candidate: the absence IS an error, so
                        // no object can replace the repeated handling — the
                        // "give the absent case an object" refactoring would
                        // mask it (review log §2.6: person is None -> KeyError)
                        if !branch_guards(&i.body) {
                            let e = counts.entry(n.id.to_string()).or_insert((0, usize::MAX));
                            e.0 += 1;
                            e.1 = e.1.min(line_of(source, i.range().start()));
                        }
                    }
                }
            }
            scan_stmts(&i.body, counts, source);
            for cl in &i.elif_else_clauses {
                scan_stmts(&cl.body, counts, source);
            }
        } else {
            match s {
                Stmt::FunctionDef(f) => scan_stmts(&f.body, counts, source),
                Stmt::ClassDef(cd) => scan_stmts(&cd.body, counts, source),
                Stmt::For(f) => scan_stmts(&f.body, counts, source),
                Stmt::While(w) => scan_stmts(&w.body, counts, source),
                Stmt::Try(t) => {
                    scan_stmts(&t.body, counts, source);
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                        scan_stmts(&eh.body, counts, source);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Is the guarded branch of a None/empty check a FAIL-FAST GUARD — it raises
/// or returns an error shape (an HTTPException, a *Response, a dict with an
/// "error" key)? A guard is not a special-case candidate: the absence IS an
/// error, so no object can replace the repeated handling — the "give the
/// absent case an object" refactoring would mask it (review log §2.6: person
/// is None -> KeyError, mine is None -> 401).
fn branch_guards(stmts: &[Stmt]) -> bool {
    for s in stmts {
        match s {
            Stmt::Raise(_) => return true,
            Stmt::Return(r) => {
                if let Some(e) = r.value.as_deref() {
                    if return_is_error(e) {
                        return true;
                    }
                }
            }
            Stmt::If(i) => {
                if branch_guards(&i.body) || i.elif_else_clauses.iter().any(|cl| branch_guards(&cl.body)) {
                    return true;
                }
            }
            Stmt::For(f) => {
                if branch_guards(&f.body) || f.orelse.iter().any(|s| branch_guards(std::slice::from_ref(s))) {
                    return true;
                }
            }
            Stmt::While(w) => {
                if branch_guards(&w.body) || w.orelse.iter().any(|s| branch_guards(std::slice::from_ref(s))) {
                    return true;
                }
            }
            Stmt::With(w) => {
                if branch_guards(&w.body) {
                    return true;
                }
            }
            Stmt::Try(t) => {
                if branch_guards(&t.body) {
                    return true;
                }
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                    if branch_guards(&eh.body) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

/// An ERROR-shaped return value: an HTTPException, any *Response object, or a
/// dict literal carrying an "error" key. A plain value (an int, a default
/// object) is a special-case candidate, not a guard.
fn return_is_error(e: &Expr) -> bool {
    match e {
        Expr::Call(c) => match c.func.as_ref() {
            Expr::Name(n) => {
                let name = n.id.as_str();
                name == "HTTPException" || name.ends_with("Response")
            }
            Expr::Attribute(a) => {
                let attr = a.attr.as_str();
                attr == "HTTPException" || attr.ends_with("Response")
            }
            _ => false,
        },
        Expr::Dict(d) => d
            .items
            .iter()
            .any(|item| matches!(&item.key, Some(Expr::StringLiteral(s)) if s.value.to_str() == "error")),
        _ => false,
    }
}
/// Latent Visitor: >= 2 functions dispatch over the SAME element family
/// (isinstance / type()== / __class__ comparisons) — the signal that the
/// OPERATIONS vary independently of the elements (GoF's visitor criterion).
/// Detection-only: the message names the refactoring; the fix (a Visitor
/// class + accept methods on every element) is structural.
///
/// The anti-thrash contract: every chain this rule claims is recorded in
/// `claimed_dispatch` so conditional-polymorphism skips it — one ruling per
/// chain. Single-operation dispatch stays polymorphism's territory.
pub fn latent_visitor_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    // per family: (dispatch-site count, first-site line)
    let mut families: std::collections::HashMap<String, (usize, usize)> = std::collections::HashMap::new();
    let mut site_lines: std::collections::HashMap<usize, String> = std::collections::HashMap::new();

    fn walk(
        stmts: &[Stmt],
        source: &str,
        families: &mut std::collections::HashMap<String, (usize, usize)>,
        site_lines: &mut std::collections::HashMap<usize, String>,
    ) {
        for s in stmts {
            if let Stmt::If(i) = s {
                let arms: Vec<&Expr> = std::iter::once(i.test.as_ref())
                    .chain(i.elif_else_clauses.iter().filter_map(|c| c.test.as_ref()))
                    .collect();
                if arms.len() >= 2 {
                    // the chain's FAMILY is the SET of types it dispatches
                    // on (sorted, deduped) — the visitor signal is the
                    // MULTI-SITE (>= 2 operations over the same type set),
                    // not the arm count; keying by the value's name would
                    // merge chains that share a variable but test different
                    // types (and could suppress a conditional-polymorphism
                    // ruling on one of them)
                    let type_keys: Vec<Option<String>> =
                        arms.iter().map(|t| dispatch_arm(t).map(|(tk, _)| tk)).collect();
                    if type_keys.iter().all(Option::is_some) {
                        let mut fam: Vec<String> = type_keys.into_iter().flatten().collect();
                        fam.sort_unstable();
                        fam.dedup();
                        let chain_family = fam.join("|");
                        let line = line_of(source, s.range().start());
                        let e = families.entry(chain_family.clone()).or_insert((0, line));
                        e.0 += 1;
                        site_lines.insert(line, chain_family);
                    }
                }
                walk(&i.body, source, families, site_lines);
                for cl in &i.elif_else_clauses {
                    walk(&cl.body, source, families, site_lines);
                }
            } else {
                match s {
                    Stmt::FunctionDef(f) => walk(&f.body, source, families, site_lines),
                    Stmt::ClassDef(cd) => walk(&cd.body, source, families, site_lines),
                    Stmt::For(f) => walk(&f.body, source, families, site_lines),
                    Stmt::While(w) => walk(&w.body, source, families, site_lines),
                    _ => {}
                }
            }
        }
    }
    walk(body, source, &mut families, &mut site_lines);
    let qualifying: std::collections::HashSet<String> = families
        .iter()
        .filter(|(_, (n, _))| *n >= 2)
        .map(|(f, _)| f.clone())
        .collect();
    for (line, family) in &site_lines {
        if qualifying.contains(family) {
            state.claimed_dispatch.insert(*line);
        }
    }
    let mut v: Vec<(&String, &(usize, usize))> = families.iter().collect();
    v.sort_by_key(|(_, (_, l))| *l);
    for (family, (n, line)) in v {
        if *n >= 2 {
            state.findings.push(Finding {
col: 0,
                file: state.file.to_string(),
                line: *line,
                function: String::new(),
                kind: "latent-visitor".into(),
                severity: "warn".into(),
                message: format!(
                    "{n} operations branch on the same object type ('{family}') — Replace Conditional with Visitor: give each type a visit_<Type> method so the branches disappear"
                ),
            });
        }
    }
}

/// The (family, dispatched type) of one chain-arm test — the discriminated
/// value and the type it dispatches on. Value comparisons (`x == 1`) return
/// None: they are not type-tag dispatch.
fn dispatch_arm(e: &Expr) -> Option<(String, String)> {
    match e {
        Expr::Call(c) => {
            if let Expr::Name(n) = c.func.as_ref() {
                if n.id.as_str() == "isinstance" {
                    let args = &c.arguments.args;
                    if args.len() >= 2 {
                        let Expr::Name(f) = &args[0] else {
                            return None;
                        };
                        // the FAMILY is the dispatched TYPE — two functions
                        // that both use `x` but test different types are
                        // different element families; keying by the value's
                        // name merged them and could suppress a legitimate
                        // conditional-polymorphism ruling on one of them
                        match &args[1] {
                            Expr::Name(t) => return Some((t.id.to_string(), f.id.to_string())),
                            Expr::Tuple(t) => {
                                let types: Vec<String> = t
                                    .elts
                                    .iter()
                                    .filter_map(|e| {
                                        if let Expr::Name(n) = e {
                                            Some(n.id.to_string())
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                if types.len() == 1 {
                                    return Some((types[0].clone(), f.id.to_string()));
                                }
                                return Some((types.join("|"), f.id.to_string()));
                            }
                            _ => return None,
                        }
                    }
                }
            }
            None
        }
        Expr::Compare(c) => {
            // type(x) == A / x.__class__ is A / x is A
            let family = match c.left.as_ref() {
                Expr::Name(n) => n.id.to_string(),
                Expr::Call(tc) => {
                    if let Expr::Name(f) = tc.func.as_ref() {
                        if f.id.as_str() == "type" && tc.arguments.args.len() == 1 {
                            if let Expr::Name(x) = &tc.arguments.args[0] {
                                x.id.to_string()
                            } else {
                                return None;
                            }
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                }
                _ => return None,
            };
            if c.ops.len() == 1 && c.comparators.len() == 1 {
                if let Expr::Name(t) = &c.comparators[0] {
                    // (type, value) — the family is the compared TYPE
                    return Some((t.id.to_string(), family));
                }
            }
            None
        }
        _ => None,
    }
}

// =====================================================================
// repo-wide families: duplicate (Dice on structural skeletons) + unused
// (defined-but-never-referenced). These are computed in the Rust runner
// across ALL files of one invocation — mirroring _duplicate_actions and
// _unused_actions in lucidlint.py.
// =====================================================================

pub use crate::common::SkeletonFn;

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
    T(&'a str),
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

// lucidlint: ignore large-function the parity-locked walker mirrors CPython's field order — one dispatch table
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
/// Dice is computed for every pair: a content hash of the unique bigram SET
/// cannot shortcut the decision — equal unique sets with different
/// multiplicities (a repeated bigram) score Dice < 0.9 while hashing equal,
/// and differing sets can still score >= 0.9. The set hash is neither
/// necessary nor sufficient, so it is not a valid collision test
/// (2026-08-17 review-log: set-vs-multiset 100%-similar false positive).
pub fn duplicate_findings(fns: &[SkeletonFn]) -> Vec<Finding> {
    use std::collections::HashMap;
    let mut out = Vec::new();
    let interner = BigramInterner::new(&fns.iter().map(|f| f.skeleton.as_slice()).collect::<Vec<_>>());
    let mut buckets: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, fr) in fns.iter().enumerate() {
        buckets.entry(fr.skeleton.len()).or_default().push(i);
    }
    let mut len_keys: Vec<usize> = buckets.keys().copied().collect();
    len_keys.sort_unstable();
    // the outer loop is independent per candidate (reads the shared buckets,
    // writes only its own result) — parallel, reassembled in index order so
    // the finding order stays deterministic
    let results: Vec<Option<Finding>> = (0..fns.len())
        .into_par_iter()
        .map(|i| {
            let fr = &fns[i];
            let l = fr.skeleton.len();
            let tol = (l / 5).max(2);
            let lo = l.saturating_sub(tol);
            let hi = l + tol;
            let mut best: Option<(usize, f64)> = None;
            for &len in len_keys.iter().filter(|&&k| k >= lo && k <= hi) {
                let bucket = &buckets[&len];
                let start = bucket.partition_point(|&j| j <= i);
                for &j in &bucket[start..] {
                    let sim = dice_from_bigrams(interner.bigrams_of(i), interner.bigrams_of(j));
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
                return Some(Finding {
col: 0,
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
            None
        })
        .collect();
    for f in results.into_iter().flatten() {
        out.push(f);
    }
    out
}

/// Interns skeleton tokens to small ids and precomputes each function's
/// sorted bigram index list ONCE — `duplicate_findings` then scores pairs
/// with an allocation-free two-pointer merge instead of rebuilding bigram
/// maps per pair (the save-time repo-wide merge's dominant cost: 59ms on a
/// 57-file repo for 224 skeletons).
pub struct BigramInterner {
    bigrams: Vec<(u32, u32)>,
    offsets: Vec<(usize, usize)>, // (start, end) into bigrams per input
}

impl BigramInterner {
    pub fn new(skeletons: &[&[String]]) -> Self {
        let mut intern: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
        let mut bigrams: Vec<(u32, u32)> = Vec::new();
        let mut offsets: Vec<(usize, usize)> = Vec::with_capacity(skeletons.len());
        for skel in skeletons {
            let mut ids: Vec<u32> = Vec::with_capacity(skel.len());
            for tok in *skel {
                let next = intern.len() as u32;
                let id = *intern.entry(tok.as_str()).or_insert(next);
                ids.push(id);
            }
            let start = bigrams.len();
            bigrams.extend(ids.windows(2).map(|w| (w[0], w[1])));
            bigrams[start..].sort_unstable();
            offsets.push((start, bigrams.len()));
        }
        Self { bigrams, offsets }
    }

    /// The function's sorted bigram list.
    pub fn bigrams_of(&self, i: usize) -> &[(u32, u32)] {
        let (s, e) = self.offsets[i];
        &self.bigrams[s..e]
    }
}

/// Dice over SORTED bigram lists — multiset intersection via a two-pointer
/// merge (counts equal runs). Matches `dice_similarity`'s multiset contract
/// exactly; the pairing test pins them equal on the review-log cases.
pub fn dice_from_bigrams(a: &[(u32, u32)], b: &[(u32, u32)]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut common = 0usize;
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            let x = a[i];
            let (ci, cj) = (i, j);
            while i < a.len() && a[i] == x {
                i += 1;
            }
            while j < b.len() && b[j] == x {
                j += 1;
            }
            common += (i - ci).min(j - cj);
        } else if a[i] < b[j] {
            i += 1;
        } else {
            j += 1;
        }
    }
    (2.0 * common as f64) / (a.len() + b.len()) as f64
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
                    "function '{name}' ({rel}:{line}) is referenced only from tests — if it is a deliberate test seam (isolation hook, fixture helper), document it with `# lucidlint: ignore unused <why>`; otherwise production code that nothing ships calls is dead — delete it"
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
            col: 0,
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
//     collections of dicts/tuples, nested lists, fixed tuples); maps pass.
//     NO boundary exemption: a wire payload is still a record — the class
//     carries a to_dict()/to_json() at the serialization edge
//   - literals: dict literals with >= 2 keys, >= 1 constant string key,
//     >= 1 dynamic value, in a record position (assign/return/yield)
// =====================================================================

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
struct RecordHit {
    line: usize,
    col: usize,
    keys: Vec<String>,
}

/// The dict-literal scan: SHAPE, not position, defines a record — every
/// expression container is descended (call arguments included; the old
/// "inline arguments are maps" carve-out hid wire-format construction sites).
/// Spread merges ({**base, ...}) update an existing shape and are not records.
fn record_literal_scan(e: &Expr, source: &str, found: &mut Vec<RecordHit>) {
    match e {
        Expr::Dict(d) => {
            if d.items.iter().any(|it| it.key.is_none()) {
                return;
            }
            let keys: Vec<String> = d
                .items
                .iter()
                .filter_map(|it| match &it.key {
                    Some(Expr::StringLiteral(s)) => Some(s.value.to_string()),
                    _ => None,
                })
                .collect();
            let has_dynamic_value = d.items.iter().any(|it| !is_constant_value(&it.value));
            if d.items.len() >= 2 && !keys.is_empty() && has_dynamic_value {
                found.push(RecordHit {
                    line: line_of(source, d.range().start()),
                    col: col_of(source, d.range().start()),
                    keys,
                });
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
        Expr::Set(s) => {
            for elt in &s.elts {
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
        Expr::Call(c) => {
            // dict(a=1, b=x) is the literal's call-form twin — a record built
            // through a call so the literal-only scan could be dodged.
            let is_dict = match c.func.as_ref() {
                Expr::Name(n) => n.id.as_str() == "dict",
                Expr::Attribute(a) => a.attr.as_str() == "dict",
                _ => false,
            };
            if is_dict {
                let kwargs = &c.arguments.keywords;
                let has_dynamic = kwargs.iter().any(|k| !is_constant_value(&k.value));
                if kwargs.len() >= 2 && has_dynamic {
                    let keys: Vec<String> = kwargs
                        .iter()
                        .filter_map(|k| k.arg.as_ref().map(|n| n.id.to_string()))
                        .collect();
                    found.push(RecordHit {
                        line: line_of(source, c.range().start()),
                        col: col_of(source, c.range().start()),
                        keys,
                    });
                }
            }
            // uniform descent: EVERY call's inline arguments are expressions
            // like any other container's
            for a in &c.arguments.args {
                record_literal_scan(a, source, found);
            }
            for k in &c.arguments.keywords {
                record_literal_scan(&k.value, source, found);
            }
        }
        _ => {}
    }
}

/// The message's key listing — source order, capped so a generated
/// 40-field record stays one line.
fn display_keys(keys: &[String]) -> String {
    const MAX: usize = 6;
    if keys.len() <= MAX {
        return keys.join(", ");
    }
    format!("{}, … (+{})", keys[..MAX].join(", "), keys.len() - MAX)
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
                        let text = source[a.range()].to_string();
                        state.findings.push(Finding {
col: 0,
                            file: state.file.to_string(),
                            line: def_line,
                            function: f.name.to_string(),
                            kind: "record-shape".into(),
                            severity: "fail".into(),
                            message: format!(
                                "bare record collection '{text}' in parameter '{arg}' of {} (line {def_line}) — convert it to a class named with a domain noun, with named fields; a wire payload is still a record — give the class a to_dict()/to_json() at the serialization edge",
                                f.name.as_str()
                            ),
                        });
                    }
                }
                if let Some(r) = &f.returns {
                    if annotation_is_record(r.as_ref()) {
                        let text = source[r.range()].to_string();
                        state.findings.push(Finding {
col: 0,
                            file: state.file.to_string(),
                            line: def_line,
                            function: f.name.to_string(),
                            kind: "record-shape".into(),
                            severity: "fail".into(),
                            message: format!(
                                "bare record collection '{text}' as return type of {} (line {def_line}) — convert it to a class named with a domain noun, with named fields; a wire payload is still a record — give the class a to_dict()/to_json() at the serialization edge",
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
    // record dict literals ANYWHERE — shape, not position, defines a record
    // (uniform descent; the record-position carve-out hid call-site wires)
    let mut found: Vec<RecordHit> = Vec::new();
    // descend EVERY node (function bodies included) — each Expr feeds the
    // scan, which recurses internally through containers
    let mut sq: Vec<&Stmt> = body.iter().collect();
    let mut si = 0usize;
    while si < sq.len() {
        // descend ALL statement kinds incl. function/class bodies
        match sq[si] {
            Stmt::FunctionDef(f) => {
                let fd_stmt = Stmt::FunctionDef(f.clone());
                for e in stmt_exprs(&fd_stmt) {
                    record_literal_scan(e, source, &mut found);
                }
                for b in &f.body {
                    sq.push(b);
                }
            }
            _ => {
                for e in stmt_exprs(sq[si]) {
                    record_literal_scan(e, source, &mut found);
                }
                push_stmt_children(sq[si], &mut sq);
            }
        }
        si += 1;
    }
    // dedupe on (line, col): the dict() call-form and a nested literal can
    // share a range — one finding per record node
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut unique: Vec<RecordHit> = Vec::new();
    for h in found {
        if seen.insert((h.line, h.col)) {
            unique.push(h);
        }
    }
    for h in unique {
        let keys = display_keys(&h.keys);
        state.findings.push(Finding { file: state.file.to_string(), line: h.line, col: h.col, function: String::new(), kind: "record-shape".into(), severity: "fail".into(), message: format!("dict with constant keys {{{keys}}} is a record — make a class named with a domain noun (fields: {keys}); a wire payload is still a record — give the class a to_dict()/to_json() — fix: extract-record-class --name <Record>") });
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
col: 0,
                file: state.file.to_string(),
                line: line_of(source, cls.name.range().start()),
                function: cls.name.to_string(),
                kind: "partition".into(),
                severity: "fail".into(),
                message: format!(
                    "methods split into {count} field-disjoint groups ({groups_text}), connectors removed: {conn_text} — each group touches only its own fields, so the class is really {count} independent classes",
                    count = groups.len()
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
col: 0,
        file: state.file.to_string(),
        line,
        function: String::new(),
        kind: "monkeypatch".into(),
        severity: "fail".into(),
        message: format!(
            "{desc} at line {line} — never monkeypatch global state; inject an object fake (a class implementing the real protocol) via parameter injection or a dependency-injection container — fakes are objects, not functions"
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
        col: 0,
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
col: 0,
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
// lucidlint: ignore large-function the fake-filesystem grammar is one decision table per backend
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
col: 0,
                        file: state.file.to_string(),
                        line,
                        function: f.name.to_string(),
                        kind: "fakefs".into(),
                        severity: "fail".into(),
                        message: format!(
                            "test '{}' at line {line} touches the real filesystem (tmp_path/open/Path) without pyfakefs — tests fake the filesystem (the `fs` fixture or fake_filesystem_unittest). Reach a real tmp_path only when the code under test needs real FS semantics (subprocess interop, symlinks, C-level I/O like sqlite3) and comment why — or mark `# lucidlint: ignore-file fakefs <why>`, citing the standard that permits real FS here",
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
                        // unittest: self.assertEqual/assertTrue/assertRaises/...
                        // and pytest: pytest.raises / self.fail
                        if a.attr.as_str().starts_with("assert")
                            || a.attr.as_str() == "raises"
                            || a.attr.as_str() == "fail"
                        {
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
                        col: 0,
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
        let mut declared: Vec<String> = Vec::new();
        for m in &cls.body {
            match m {
                Stmt::AnnAssign(a) => {
                    if let Expr::Name(n) = a.target.as_ref() {
                        declared.push(n.id.to_string());
                    }
                }
                Stmt::Assign(a) => {
                    for t in &a.targets {
                        if let Expr::Name(n) = t {
                            if n.id.as_str() == "__slots__" {
                                if let Some(tuple) = slot_names(&a.value) {
                                    declared.extend(tuple);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        for m in &cls.body {
            let Stmt::FunctionDef(f) = m else { continue };
            if f.name.as_str() == "__init__" {
                for st in &f.body {
                    if let Stmt::AnnAssign(a) = st {
                        if let Expr::Attribute(at) = a.target.as_ref() {
                            if matches!(at.value.as_ref(), Expr::Name(n) if n.id.as_str() == "self") {
                                declared.push(at.attr.to_string());
                            }
                        }
                    }
                }
            }
        }
        declared.sort();
        declared.dedup();
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
            declared,
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
col: 0,
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

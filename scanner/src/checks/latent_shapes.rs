//! The latent-class shape families: a method in exile, an anonymous tuple
//! record, a fixed-arity tuple, and a state-threading assembly site.
// lucidlint: ignore-file complexity the shape walkers are dispatch tables over the
// statement and expression kinds — arm count is the AST's surface, not branching

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::finding::Finding;

use super::{all_exprs, class_attr_names, push_stmt_children, stmt_exprs};
use crate::source::Source;
use crate::ScanState;

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
        let line = Source::line_of(state.source, f.name.range().start());
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
                        state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
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
                                                    ), });
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
                        Source::line_of(state.source, a.range().start()),
                        f.name.as_str(),
                    );
                }
                emit_wide_tuple(
                    state,
                    f.returns.as_deref(),
                    &format!("the return type of '{}'", f.name),
                    Source::line_of(state.source, f.name.range().start()),
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
                        Source::line_of(state.source, a.target.range().start()),
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
        "{what} is a {n}-tuple — the positions carry meaning the call site cannot see; introduce a record named with a domain noun, with named fields — fix: extract-class"
    );
    // 4+ elements fail; exactly 3 warns (the RGB gray zone). Two pushes so
    // the kind/severity literal pair stays adjacent — `make rules`' drift
    // detector reads the pair from the construction.
    if n >= 4 {
        state.findings.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
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
            logical_start: None,
            seam_members: Vec::new(),
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
                        let line = Source::line_of(state.source, n.range().start());
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
            state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                            file: state.file.to_string(),
                            line: *line,
                            function: String::new(),
                            kind: "tuple-record".into(),
                            severity: "fail".into(),
                            message: format!(
                                "the values of '{name}' are {arity}-tuples read {reads} times by constant index — an anonymous record; make it a class named with a domain noun (apply: lucidlint fix --kind tuple-record --name <N>) — fix: tuple-record"
                            ), });
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

// ------------------------------------------------------------- latent-class
// the declaration/ownership half of the family: shared parameter pairs,
// cross-object field reads, and quiet member assignment.

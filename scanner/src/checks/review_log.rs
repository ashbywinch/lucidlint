//! Review-log rules (#26–#34 round, family-album log §10/§11): duplicate
//! module-scope definitions, restating docstrings, and duplicated statement
//! blocks — all deterministic on the AST the scan already walks.

use std::collections::HashSet;

use ruff_python_ast::{AnyNodeRef, Expr, Stmt};

use crate::{stmt_line, Finding, ScanState};

use super::{skel_children, Q};

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
        for (name, is_def, stub) in bound_names(s, &overload_bound) {
            if name.is_empty() {
                continue;
            }
            if is_def {
                let exempt = stub || (overloaded.contains(&name) && impl_def_line.get(&name).copied() == Some(line));
                if !exempt {
                    if let Some((_, first_line)) = seen.iter().find(|(n, _)| *n == name) {
                        state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                                    file: state.file.to_string(),
                                                    line,
                                                    function: name.clone(),
                                                    kind: "duplicate-def".into(),
                                                    severity: "fail".into(),
                                                    message: format!(
                                                        "module-scope name '{name}' is bound twice (first at line {first_line}) — the later definition shadows the earlier: a shadowing hazard (dispatch or edit mistake); rename one — fix: duplicate-def"
                                                    ), });
                    }
                }
            }
            seen.push((name, line));
        }
    }
}

/// The names one module-scope statement binds: `(name, is the binder a
/// def/class?, is it an @overload stub?)`.
fn bound_names(s: &Stmt, overload_bound: &HashSet<String>) -> Vec<(String, bool, bool)> {
    let mut bindings: Vec<(String, bool, bool)> = Vec::new();
    match s {
        Stmt::FunctionDef(f) => bindings.push((f.name.to_string(), true, is_overload_stub(f, overload_bound))),
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
                // only a DEF/CLASS shadow is a hazard worth flagging: `import
                // urllib` + `import urllib.request` both bind `urllib`
                // idiomatically, and the §10 def-in-imports mistake is exactly
                // a def landing on an import's name
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
    bindings
}

/// Is this def one of the `@overload` stubs the exemption covers?
fn is_overload_stub(f: &ruff_python_ast::StmtFunctionDef, overload_bound: &HashSet<String>) -> bool {
    f.decorator_list.iter().any(|d| {
        matches!(&d.expression, Expr::Name(n) if overload_bound.contains(n.id.as_str()))
            || matches!(&d.expression, Expr::Attribute(a) if a.attr.as_str() == "overload")
    })
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
            state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                            file: state.file.to_string(),
                            line,
                            function: String::new(),
                            kind: "restating-docstring".into(),
                            severity: "warn".into(),
                            message: format!(
                                "docstring restates the body — {covered}/{} content words appear in the code; name the concept instead — fix: restating-docstring",
                                words.len()
                            ), });
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
                    state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                            file: state.file.to_string(),
                                            line,
                                            function: String::new(),
                                            kind: "duplicate-block".into(),
                                            severity: "warn".into(),
                                            message: format!(
                                                "a {BLOCK_WINDOW}-statement block appears twice in this function — duplicated work. If one block is a paste copy of the other, delete it. If these are intentional twins (parallel branches), the duplication is still the finding — extract the shared part. If a twin can diverge, it is a fork; if it cannot, it is a paste.{directive}"
                                            ), });
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

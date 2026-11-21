//! `duplicate-block`: the same three-statement block twice in one function —
//! one copy drifts from the other.

use ruff_python_ast::{AnyNodeRef, Stmt};

use crate::{finding::Finding, source::Source, ScanState};

use super::{skel_children, Q};

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
                    let line = Source::stmt_line(state.source, flat[j]);
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

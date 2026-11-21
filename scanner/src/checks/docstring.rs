//! `restating-docstring`: a docstring whose words all appear in the body —
//! the prose adds nothing the code does not already say.

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt};

use crate::{finding::Finding, source::Source, ScanState};

/// `restating-docstring`: the rule object. Its methods read one docstring; the evidence and the message live together.
/// The word length that counts as content ("the", "and" are not), the word
/// count that makes a docstring worth comparing, and the share of the body
/// tokens it must cover before it merely restates the code.
const WORD_LEN_MIN: usize = 3;
const WORDS_MIN: usize = 5;
const COVERAGE_MIN: f64 = 0.9;

pub struct Docstring;

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

impl Docstring {
    fn docstring_content_words(doc: &str) -> Vec<String> {
        doc.split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= WORD_LEN_MIN && !DOCSTRING_STOPWORDS.contains(w))
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
    pub fn findings(state: &mut ScanState, module_body: &[Stmt]) {
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
            let words = Self::docstring_content_words(doc);
            if words.len() < WORDS_MIN {
                continue; // short docstrings carry the name; not noise
            }
            let tokens = Self::body_tokens(&body[1..]);
            let covered = words.iter().filter(|w| tokens.contains(w.as_str())).count();
            if covered as f64 / words.len() as f64 >= COVERAGE_MIN {
                let line = Source::stmt_line(state.source, s);
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
}

//! `test-asserts-source`: a test asserts on code text instead of on the
//! behaviour under test. A text match pins one spelling, so it passes even
//! when the code does nothing.
//!
//! The rule is content-based: a code-shaped literal in the assertion, or a
//! name that holds code text (a code literal, an `include_str!` of a source
//! file, or a read of a source file), is the finding. It does not care what a
//! repository names its inputs, and it applies to the code's output as well.

use ruff_python_ast::{CmpOp, Expr, Stmt};
use ruff_text_size::Ranged;
use std::collections::HashSet;

use crate::code_text::CodeText;
use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

const MESSAGE: &str = "asserts on code text, not on the behaviour under test — a text match pins one spelling, so it passes even when the code does nothing. Keep the test: assert what the code actually does instead. Do not delete the test or weaken it to match the text.";

fn message(function: &str) -> String {
    let subject = if function.is_empty() {
        "this test".to_string()
    } else {
        format!("test '{function}'")
    };
    format!("{subject} {MESSAGE}")
}

/// The test-quality pass: every assertion whose subject is code text.
pub fn test_asserts_source_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    walk_body(state, body, source, "", &mut CodeNames::default());
}

/// The names that hold code text or a source path, inferred from assignment.
#[derive(Clone, Default)]
struct CodeNames {
    code: HashSet<String>,
    paths: HashSet<String>,
}

impl CodeNames {
    fn record(&mut self, name: &str, value: &Expr) {
        if is_code_literal(value) || self.reads_code_text(value) {
            self.code.insert(name.to_string());
            self.paths.remove(name);
        } else if code_path_of(value) {
            self.paths.insert(name.to_string());
            self.code.remove(name);
        } else if let Expr::Name(other) = value {
            self.copy(name, other.id.as_str());
        } else {
            self.code.remove(name);
            self.paths.remove(name);
        }
    }

    fn copy(&mut self, name: &str, other: &str) {
        if self.code.contains(other) {
            self.code.insert(name.to_string());
        } else {
            self.code.remove(name);
        }
        if self.paths.contains(other) {
            self.paths.insert(name.to_string());
        } else {
            self.paths.remove(name);
        }
    }

    fn is_code(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Name(name) if self.code.contains(name.id.as_str()))
    }

    fn is_code_path(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Name(name) if self.paths.contains(name.id.as_str()))
    }

    fn reads_code_text(&self, value: &Expr) -> bool {
        let Expr::Call(call) = value else {
            return false;
        };
        let Expr::Attribute(attribute) = call.func.as_ref() else {
            return false;
        };
        if !matches!(attribute.attr.as_str(), "read_text" | "read" | "read_bytes") {
            return false;
        }
        code_path_of(&attribute.value) || self.is_code_path(&attribute.value)
    }
}

fn walk_body(state: &mut ScanState, body: &[Stmt], source: &str, function: &str, names: &mut CodeNames) {
    for stmt in body {
        match stmt {
            Stmt::Assign(a) => {
                if let [Expr::Name(name)] = a.targets.as_slice() {
                    names.record(name.id.as_str(), &a.value);
                }
            }
            Stmt::AnnAssign(a) => {
                if let Expr::Name(name) = a.target.as_ref() {
                    match a.value.as_deref() {
                        Some(value) => names.record(name.id.as_str(), value),
                        None => {
                            names.code.remove(name.id.as_str());
                            names.paths.remove(name.id.as_str());
                        }
                    }
                }
            }
            Stmt::Assert(a) if assertion_reads_code(&a.test, names) => {
                record(state, source, a.range().start(), function);
            }
            // an assertion helper imported by name is an expression statement,
            // not an `assert` (unittest's `assertEqual(src, "…")`)
            Stmt::Expr(e) if assertion_reads_code(&e.value, names) => {
                record(state, source, e.range().start(), function);
            }
            Stmt::FunctionDef(f) => {
                let mut local = names.clone();
                walk_body(state, &f.body, source, f.name.as_str(), &mut local);
            }
            Stmt::ClassDef(c) => {
                let mut class = names.clone();
                walk_body(state, &c.body, source, function, &mut class);
            }
            Stmt::If(i) => {
                for body in std::iter::once(&i.body).chain(i.elif_else_clauses.iter().map(|clause| &clause.body)) {
                    walk_body(state, body, source, function, names);
                }
            }
            Stmt::For(f) => {
                walk_body(state, &f.body, source, function, names);
                walk_body(state, &f.orelse, source, function, names);
            }
            Stmt::While(w) => {
                walk_body(state, &w.body, source, function, names);
                walk_body(state, &w.orelse, source, function, names);
            }
            Stmt::With(w) => walk_body(state, &w.body, source, function, names),
            Stmt::Try(t) => {
                walk_body(state, &t.body, source, function, names);
                for handler in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                    walk_body(state, &h.body, source, function, names);
                }
                walk_body(state, &t.orelse, source, function, names);
                walk_body(state, &t.finalbody, source, function, names);
            }
            Stmt::Match(m) => {
                for case in &m.cases {
                    walk_body(state, &case.body, source, function, names);
                }
            }
            _ => {}
        }
    }
}

fn record(state: &mut ScanState, source: &str, offset: ruff_text_size::TextSize, function: &str) {
    state.findings.push(Finding {
        logical_start: None,
        seam_members: Vec::new(),
        col: 0,
        file: state.file.to_string(),
        line: Source::line_of(source, offset),
        function: function.to_string(),
        kind: "test-asserts-source".into(),
        severity: "warn".into(),
        message: message(function),
    });
}

fn is_code_literal(value: &Expr) -> bool {
    matches!(value, Expr::StringLiteral(literal) if CodeText::is_python_fragment(literal.value.to_str()))
}

/// The expression names a source file by extension: a string literal, a
/// `Path(...)` call, or a path built from either.
fn code_path_of(value: &Expr) -> bool {
    match value {
        Expr::StringLiteral(literal) => CodeText::is_python_path(literal.value.to_str()),
        Expr::Call(call) => call.arguments.args.iter().any(code_path_of),
        Expr::BinOp(binary) => code_path_of(&binary.left) || code_path_of(&binary.right),
        Expr::Subscript(subscript) => code_path_of(&subscript.value) || code_path_of(&subscript.slice),
        Expr::Tuple(tuple) => tuple.elts.iter().any(code_path_of),
        _ => false,
    }
}

/// A value whose content an assertion can search. An attribute or call is an
/// unknown type (`finding.message`, `str(finding)`), not a code string.
fn is_searchable_string(expr: &Expr) -> bool {
    matches!(expr, Expr::Name(_) | Expr::Subscript(_) | Expr::BinOp(_))
}

fn assertion_reads_code(test: &Expr, names: &CodeNames) -> bool {
    match test {
        Expr::Compare(cmp) if cmp.ops.len() == 1 => compare_reads_code(cmp, names),
        Expr::Call(call) => call_reads_code(call, names),
        Expr::UnaryOp(unary) => assertion_reads_code(&unary.operand, names),
        Expr::BoolOp(boolean) => boolean.values.iter().any(|value| assertion_reads_code(value, names)),
        _ => false,
    }
}

fn compare_reads_code(cmp: &ruff_python_ast::ExprCompare, names: &CodeNames) -> bool {
    let left = &cmp.left;
    let right = &cmp.comparators[0];
    match cmp.ops[0] {
        CmpOp::In | CmpOp::NotIn => search_reads_code(left, right, names),
        CmpOp::Eq | CmpOp::NotEq => equality_reads_code(left, right, names),
        _ => false,
    }
}

fn search_reads_code(needle: &Expr, haystack: &Expr, names: &CodeNames) -> bool {
    is_searchable_string(haystack) && (is_code_literal(needle) || names.is_code(needle) || names.is_code(haystack))
}

fn equality_reads_code(left: &Expr, right: &Expr, names: &CodeNames) -> bool {
    (is_code_literal(left) && is_searchable_string(right))
        || (is_code_literal(right) && is_searchable_string(left))
        || (is_code_literal(left) && is_code_literal(right))
        || names.is_code(left)
        || names.is_code(right)
}

/// unittest's assertion methods and helpers that inspect their argument:
/// `assertIn(member, text)`, `assertEqual(text, literal)`, `assertRegex(text, ..)`,
/// and `assertTrue(<expr>)` recurse into the expression.
fn call_reads_code(call: &ruff_python_ast::ExprCall, names: &CodeNames) -> bool {
    let method = match call.func.as_ref() {
        Expr::Attribute(attribute) => attribute.attr.as_str(),
        Expr::Name(name) => name.id.as_str(),
        _ => return false,
    };
    let args = call.arguments.args.as_ref();
    match method {
        "assertIn" | "assertNotIn" => args.len() == 2 && search_reads_code(&args[0], &args[1], names),
        "assertEqual" | "assertNotEqual" => args.len() == 2 && equality_reads_code(&args[0], &args[1], names),
        "assertStartsWith" | "assertEndsWith" => args.len() == 2 && search_reads_code(&args[1], &args[0], names),
        "assertRegex" | "assertNotRegex" => args.first().is_some_and(|arg| names.is_code(arg)),
        "assertTrue" | "assertFalse" => args.first().is_some_and(|arg| assertion_reads_code(arg, names)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::CodeNames;
    use ruff_python_ast::Stmt;
    use ruff_python_parser::parse_module;

    fn names_from(source: &str) -> CodeNames {
        let parsed = parse_module(source).expect("test source parses");
        let mut names = CodeNames::default();
        if let Some(Stmt::Assign(assign)) = parsed.syntax().body.first() {
            if let [ruff_python_ast::Expr::Name(name)] = assign.targets.as_slice() {
                names.record(name.id.as_str(), &assign.value);
            }
        }
        names
    }

    #[test]
    fn a_code_literal_marks_a_name_as_code() {
        let names = names_from("src = \"def f():\\n    pass\\n\"\n");
        assert!(names.code.contains("src"));
    }

    #[test]
    fn a_source_file_read_marks_a_name_as_code() {
        let names = names_from("src = Path(\"src/app.py\").read_text()\n");
        assert!(names.code.contains("src"));
    }

    #[test]
    fn a_data_file_read_does_not_mark_a_name_as_code() {
        let names = names_from("table = Path(\"table.md\").read_text()\n");
        assert!(!names.code.contains("table"));
    }
}

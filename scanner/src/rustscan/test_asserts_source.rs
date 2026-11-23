//! `test-asserts-source` for Rust: a test asserts on code text instead of on
//! the parsed finding or the code's output. A text match pins one spelling, so
//! it passes even when the code does nothing.
//!
//! The rule is content-based: a code-shaped literal in the assertion, or a
//! name that holds code text (a code literal, an `include_str!` of a source
//! file, or a read of a source file), is the finding. It does not care what a
//! repository calls its fixtures, and it applies to a fixer's output too.

use std::collections::HashSet;

use syn::spanned::Spanned;
use syn::{Expr, File, Item, Lit, LitStr, Macro, Stmt};

use crate::code_text::CodeText;
use crate::finding::Finding;

use super::attrs::Attrs;

const MESSAGE: &str = "asserts on code text, not on the behaviour under test — a text match pins one spelling, so it passes even when the code does nothing. Keep the test: assert the parsed result or the observable behaviour instead. Do not delete the test or weaken it to match the text.";

fn message(function: &str) -> String {
    let subject = if function.is_empty() {
        "this test".to_string()
    } else {
        format!("test '{function}'")
    };
    format!("{subject} {MESSAGE}")
}

/// Every test function — `#[test]` fns and helpers inside `#[cfg(test)]`
/// modules — is walked for an assertion on code text.
pub(crate) fn test_asserts_source_findings(file: &File, file_name: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for item in &file.items {
        walk_item(item, file_name, false, &mut out);
    }
    out
}

fn walk_item(item: &Item, file_name: &str, in_test_module: bool, out: &mut Vec<Finding>) {
    match item {
        Item::Fn(f) => {
            let is_test = in_test_module || Attrs::has_attr(&f.attrs, "test");
            if is_test {
                walk_block(&f.block, f.sig.ident.to_string(), file_name, out);
            }
        }
        Item::Mod(m) => {
            if let Some((_, items)) = &m.content {
                let test_module = in_test_module || Attrs::has_cfg_test(&m.attrs);
                for child in items {
                    walk_item(child, file_name, test_module, out);
                }
            }
        }
        _ => {}
    }
}

fn walk_block(block: &syn::Block, function: String, file_name: &str, out: &mut Vec<Finding>) {
    let mut names = CodeNames::default();
    walk_statements(&block.stmts, &function, file_name, &mut names, out);
}

fn walk_statements(stmts: &[Stmt], function: &str, file_name: &str, names: &mut CodeNames, out: &mut Vec<Finding>) {
    for stmt in stmts {
        match stmt {
            Stmt::Local(local) => {
                if let Some(init) = &local.init {
                    if let syn::Pat::Ident(ident) = &local.pat {
                        names.record(ident.ident.to_string().as_str(), &init.expr);
                    }
                }
            }
            Stmt::Macro(m) => check_assert_macro(&m.mac, function, file_name, names, out),
            Stmt::Expr(expr, _) => walk_expr(expr, function, file_name, names, out),
            Stmt::Item(_) => {}
        }
    }
}

/// The names that hold code text or a source path, inferred from `let`.
#[derive(Clone, Default)]
struct CodeNames {
    code: HashSet<String>,
    paths: HashSet<String>,
}

impl CodeNames {
    fn record(&mut self, name: &str, value: &Expr) {
        if is_code_literal(value) || is_include_str_code_expr(value) || self.reads_code_text(value) {
            self.code.insert(name.to_string());
            self.paths.remove(name);
        } else if code_path_of(value) || self.is_path_name(value) {
            self.paths.insert(name.to_string());
            self.code.remove(name);
        } else if let Expr::Path(path) = value {
            if let Some(ident) = path.path.get_ident() {
                self.copy(name, ident.to_string().as_str());
            } else {
                self.clear(name);
            }
        } else {
            self.clear(name);
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

    fn clear(&mut self, name: &str) {
        self.code.remove(name);
        self.paths.remove(name);
    }

    fn is_code(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Path(path) if path.path.get_ident().is_some_and(|id| self.code.contains(id.to_string().as_str())))
    }

    fn is_path_name(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Path(path) if path.path.get_ident().is_some_and(|id| self.paths.contains(id.to_string().as_str())))
    }

    fn reads_code_text(&self, value: &Expr) -> bool {
        match value {
            Expr::Call(call) => {
                let read = path_last(&call.func).is_some_and(|name| matches!(name.as_str(), "read_to_string" | "read"));
                read && call.args.iter().any(|arg| code_path_of(arg) || self.is_path_name(arg))
            }
            Expr::MethodCall(call) => {
                let method = call.method.to_string();
                if matches!(method.as_str(), "read_to_string" | "read") {
                    return code_path_of(&call.receiver) || self.is_path_name(&call.receiver);
                }
                // a read is usually unwrapped, trimmed, or converted before use
                matches!(
                    method.as_str(),
                    "unwrap" | "expect" | "to_string" | "to_owned" | "clone" | "trim"
                ) && self.reads_code_text(&call.receiver)
            }
            _ => false,
        }
    }
}

fn is_include_str_code_expr(value: &Expr) -> bool {
    matches!(value, Expr::Macro(m) if is_include_str_code(&m.mac))
}

fn path_last(func: &Expr) -> Option<String> {
    let Expr::Path(path) = func else {
        return None;
    };
    path.path.segments.last().map(|segment| segment.ident.to_string())
}

fn is_code_literal(value: &Expr) -> bool {
    matches!(value, Expr::Lit(lit) if matches!(&lit.lit, Lit::Str(text) if CodeText::is_rust_fragment(text.value().as_str())))
}

fn code_path_of(value: &Expr) -> bool {
    match value {
        Expr::Lit(lit) => matches!(&lit.lit, Lit::Str(path) if CodeText::is_rust_path(path.value().as_str())),
        Expr::Call(call) => call.args.iter().any(code_path_of),
        Expr::Binary(binary) => code_path_of(&binary.left) || code_path_of(&binary.right),
        Expr::Reference(reference) => code_path_of(&reference.expr),
        Expr::Paren(paren) => code_path_of(&paren.expr),
        Expr::Index(index) => code_path_of(&index.expr),
        _ => false,
    }
}

fn is_include_str_code(mac: &Macro) -> bool {
    let Some(last) = mac.path.segments.last() else {
        return false;
    };
    if last.ident != "include_str" {
        return false;
    }
    syn::parse2::<LitStr>(mac.tokens.clone())
        .ok()
        .is_some_and(|path| CodeText::is_rust_path(path.value().as_str()))
}

fn check_assert_macro(mac: &Macro, function: &str, file_name: &str, names: &CodeNames, out: &mut Vec<Finding>) {
    let Some(last) = mac.path.segments.last() else {
        return;
    };
    let name = last.ident.to_string();
    let reads_code = match name.as_str() {
        "assert" | "debug_assert" => {
            syn::parse2::<Expr>(mac.tokens.clone()).is_ok_and(|expr| expression_reads_code(&expr, names))
        }
        "assert_eq" | "assert_ne" | "debug_assert_eq" | "debug_assert_ne" => syn::parse::Parser::parse2(
            syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated,
            mac.tokens.clone(),
        )
        .is_ok_and(|args| pair_reads_code(&args, names)),
        _ => false,
    };
    if reads_code {
        out.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            file: file_name.to_string(),
            line: mac.span().start().line,
            col: 0,
            function: function.to_string(),
            kind: "test-asserts-source".into(),
            severity: "warn".into(),
            message: message(function),
        });
    }
}

fn pair_reads_code(args: &syn::punctuated::Punctuated<Expr, syn::Token![,]>, names: &CodeNames) -> bool {
    let mut iter = args.iter();
    let (Some(left), Some(right)) = (iter.next(), iter.next()) else {
        return false;
    };
    equality_reads_code(left, right, names)
}

fn walk_expr(expr: &Expr, function: &str, file_name: &str, names: &mut CodeNames, out: &mut Vec<Finding>) {
    match expr {
        Expr::If(e) => {
            walk_statements(&e.then_branch.stmts, function, file_name, names, out);
            if let Some((_, else_branch)) = &e.else_branch {
                walk_expr(else_branch, function, file_name, names, out);
            }
        }
        Expr::ForLoop(e) => walk_statements(&e.body.stmts, function, file_name, names, out),
        Expr::While(e) => walk_statements(&e.body.stmts, function, file_name, names, out),
        Expr::Loop(e) => walk_statements(&e.body.stmts, function, file_name, names, out),
        Expr::Block(e) => walk_statements(&e.block.stmts, function, file_name, names, out),
        Expr::Match(e) => {
            for arm in &e.arms {
                walk_expr(&arm.body, function, file_name, names, out);
            }
        }
        Expr::Macro(m) => check_assert_macro(&m.mac, function, file_name, names, out),
        _ => {}
    }
}

fn expression_reads_code(expr: &Expr, names: &CodeNames) -> bool {
    match expr {
        Expr::MethodCall(call) => {
            let search = matches!(
                call.method.to_string().as_str(),
                "contains" | "starts_with" | "ends_with"
            );
            if !search {
                return false;
            }
            let needle = call.args.first();
            is_searchable_string(&call.receiver)
                && (needle.is_some_and(is_code_literal) || names.is_code(&call.receiver))
        }
        Expr::Binary(binary) => match binary.op {
            syn::BinOp::Eq(_) | syn::BinOp::Ne(_) => equality_reads_code(&binary.left, &binary.right, names),
            _ => false,
        },
        Expr::Unary(unary) => matches!(unary.op, syn::UnOp::Not(_)) && expression_reads_code(&unary.expr, names),
        Expr::Paren(paren) => expression_reads_code(&paren.expr, names),
        _ => false,
    }
}

fn equality_reads_code(left: &Expr, right: &Expr, names: &CodeNames) -> bool {
    (is_code_literal(left) && is_searchable_string(right))
        || (is_code_literal(right) && is_searchable_string(left))
        || (is_code_literal(left) && is_code_literal(right))
        || names.is_code(left)
        || names.is_code(right)
}

fn is_searchable_string(expr: &Expr) -> bool {
    matches!(expr, Expr::Path(_) | Expr::Index(_))
}

#[cfg(test)]
mod tests {
    use super::CodeNames;
    use syn::parse_quote;

    #[test]
    fn a_code_literal_marks_a_name_as_code() {
        let mut names = CodeNames::default();
        let value: syn::Expr = parse_quote!("def f():\n    pass\n");
        names.record("src", &value);
        assert!(names.code.contains("src"));
    }

    #[test]
    fn a_source_file_read_marks_a_name_as_code() {
        let mut names = CodeNames::default();
        let value: syn::Expr = parse_quote!(std::fs::read_to_string("src/lib.rs").unwrap());
        names.record("src", &value);
        assert!(names.code.contains("src"));
    }

    #[test]
    fn a_data_file_read_does_not_mark_a_name_as_code() {
        let mut names = CodeNames::default();
        let value: syn::Expr = parse_quote!(std::fs::read_to_string("table.md").unwrap());
        names.record("table", &value);
        assert!(!names.code.contains("table"));
    }
}

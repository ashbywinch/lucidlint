//! `test-asserts-source` for Rust: a test asserts on the source text it fed the
//! code instead of on the parsed finding or the code's output. A source
//! assertion pins the input, so it passes even when the code does nothing.

use std::collections::HashSet;

use syn::spanned::Spanned;
use syn::{Expr, File, Item, Lit, Macro, Stmt};

use crate::finding::Finding;

use super::attrs::Attrs;

const MESSAGE: &str = "this test asserts on the source text it fed the code, not on the behaviour under test — a source assertion pins the input, so it passes even when the code does nothing. Assert the parsed finding/output instead.";

/// Every test function — `#[test]` fns and helpers inside `#[cfg(test)]`
/// modules — is walked for an assertion on a fixture/source read.
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
    let mut origins = HashSet::new();
    walk_statements(&block.stmts, &function, file_name, &mut origins, out);
}

fn walk_statements(
    stmts: &[Stmt],
    function: &str,
    file_name: &str,
    origins: &mut HashSet<String>,
    out: &mut Vec<Finding>,
) {
    for stmt in stmts {
        match stmt {
            Stmt::Local(local) => bind_local(local, origins),
            Stmt::Macro(m) => check_assert_macro(&m.mac, function, file_name, origins, out),
            Stmt::Expr(expr, _) => walk_expr(expr, function, file_name, origins, out),
            Stmt::Item(_) => {}
        }
    }
}

fn bind_local(local: &syn::Local, origins: &mut HashSet<String>) {
    let Some(init) = &local.init else {
        return;
    };
    let syn::Pat::Ident(ident) = &local.pat else {
        return;
    };
    let name = ident.ident.to_string();
    if is_source_read(&init.expr) {
        origins.insert(name);
    } else {
        origins.remove(&name);
    }
}

/// A fixture helper or a file read — the value is the test's INPUT.
fn is_source_read(expr: &Expr) -> bool {
    match expr {
        Expr::Call(call) => match &*call.func {
            Expr::Path(path) => path.path.segments.last().is_some_and(|s| {
                let name = s.ident.to_string();
                name.ends_with("fixture") || name == "read_to_string" || name == "read"
            }),
            _ => false,
        },
        Expr::MethodCall(call) => matches!(call.method.to_string().as_str(), "read_to_string" | "read"),
        Expr::Macro(m) => m.mac.path.segments.last().is_some_and(|s| s.ident == "include_str"),
        _ => false,
    }
}

fn check_assert_macro(mac: &Macro, function: &str, file_name: &str, origins: &HashSet<String>, out: &mut Vec<Finding>) {
    let Some(last) = mac.path.segments.last() else {
        return;
    };
    let name = last.ident.to_string();
    let reads_source = match name.as_str() {
        "assert" | "debug_assert" => {
            syn::parse2::<Expr>(mac.tokens.clone()).is_ok_and(|expr| expression_reads_source(&expr, origins))
        }
        "assert_eq" | "assert_ne" | "debug_assert_eq" | "debug_assert_ne" => syn::parse::Parser::parse2(
            syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated,
            mac.tokens.clone(),
        )
        .is_ok_and(|args| pair_reads_source(&args, origins)),
        _ => false,
    };
    if reads_source {
        out.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            file: file_name.to_string(),
            line: mac.span().start().line,
            col: 0,
            function: function.to_string(),
            kind: "test-asserts-source".into(),
            severity: "warn".into(),
            message: MESSAGE.to_string(),
        });
    }
}

fn pair_reads_source(args: &syn::punctuated::Punctuated<Expr, syn::Token![,]>, origins: &HashSet<String>) -> bool {
    let mut iter = args.iter();
    let (Some(left), Some(right)) = (iter.next(), iter.next()) else {
        return false;
    };
    (is_source_name(left, origins) && is_text_literal(right))
        || (is_text_literal(left) && is_source_name(right, origins))
}

/// Recurse through expression bodies so an assertion inside a loop or an `if`
/// is still judged, with the fixture bindings collected before it.
fn walk_expr(expr: &Expr, function: &str, file_name: &str, origins: &mut HashSet<String>, out: &mut Vec<Finding>) {
    match expr {
        Expr::If(e) => {
            walk_statements(&e.then_branch.stmts, function, file_name, origins, out);
            if let Some((_, else_branch)) = &e.else_branch {
                walk_expr(else_branch, function, file_name, origins, out);
            }
        }
        Expr::ForLoop(e) => walk_statements(&e.body.stmts, function, file_name, origins, out),
        Expr::While(e) => walk_statements(&e.body.stmts, function, file_name, origins, out),
        Expr::Loop(e) => walk_statements(&e.body.stmts, function, file_name, origins, out),
        Expr::Block(e) => walk_statements(&e.block.stmts, function, file_name, origins, out),
        Expr::Match(e) => {
            for arm in &e.arms {
                walk_expr(&arm.body, function, file_name, origins, out);
            }
        }
        Expr::Macro(m) => check_assert_macro(&m.mac, function, file_name, origins, out),
        _ => {}
    }
}

fn expression_reads_source(expr: &Expr, origins: &HashSet<String>) -> bool {
    match expr {
        Expr::MethodCall(call) => {
            matches!(
                call.method.to_string().as_str(),
                "contains" | "starts_with" | "ends_with"
            ) && is_source_name(&call.receiver, origins)
        }
        Expr::Binary(binary) => match binary.op {
            syn::BinOp::Eq(_) | syn::BinOp::Ne(_) => {
                (is_source_name(&binary.left, origins) && is_text_literal(&binary.right))
                    || (is_text_literal(&binary.left) && is_source_name(&binary.right, origins))
            }
            _ => false,
        },
        Expr::Unary(unary) => matches!(unary.op, syn::UnOp::Not(_)) && expression_reads_source(&unary.expr, origins),
        Expr::Paren(paren) => expression_reads_source(&paren.expr, origins),
        _ => false,
    }
}

fn is_source_name(expr: &Expr, origins: &HashSet<String>) -> bool {
    matches!(expr, Expr::Path(path) if path.path.get_ident().is_some_and(|id| origins.contains(&id.to_string())))
}

fn is_text_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::Lit(lit) if matches!(lit.lit, Lit::Str(_)))
}

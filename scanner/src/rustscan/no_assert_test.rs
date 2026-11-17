//! `no-assert-test` for Rust: a `#[test]` fn with no assertion — it can never
//! fail, so it proves only that the code runs.

use syn::spanned::Spanned;
use syn::{Block, ExprMacro, File, Item, ItemFn, Path};

use crate::finding::Finding;

use super::attrs::has_attr;

/// `#[test]` + `#[ignore]` — a skipped test rots, the skipif analog. Runs as a
/// pre-pass so cfg(test) containment can't hide it.
/// `#[test]` fn with no assertion anywhere in its body — a test that can
/// never fail. Pre-pass (cfg(test) containment can't hide it); `#[ignore]`d
/// tests are skipped (they are already skipif findings) and
/// `#[should_panic]` is an assertion contract.
pub(crate) fn no_assert_test_findings(file: &File, file_name: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for item in &file.items {
        walk_test_fns(item, &mut out, file_name);
    }
    out
}

pub(crate) fn walk_test_fns(item: &Item, out: &mut Vec<Finding>, file_name: &str) {
    match item {
        Item::Fn(f) => {
            if is_unasserted_test(f) {
                out.push(Finding {
                    logical_start: None,
                    seam_members: Vec::new(),
                    file: file_name.to_string(),
                    line: f.sig.span().start().line,
                    col: 0,
                    function: f.sig.ident.to_string(),
                    kind: "no-assert-test".into(),
                    severity: "fail".into(),
                    message: "test has no assertion — it can never fail".into(),
                });
            }
        }
        Item::Mod(m) => {
            if let Some((_, items)) = &m.content {
                for it in items {
                    walk_test_fns(it, out, file_name);
                }
            }
        }
        _ => {}
    }
}

/// Any assert!/assert_eq!/.../panic! macro in the block (nested included).
fn is_assertion_macro(path: &Path) -> bool {
    let last = path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
    matches!(
        last.as_str(),
        "assert"
            | "assert_eq"
            | "assert_ne"
            | "assert_matches"
            | "debug_assert"
            | "debug_assert_eq"
            | "panic"
            | "unreachable"
            | "todo"
            | "unimplemented"
    )
}

fn block_asserts(block: &Block) -> bool {
    struct AssertProbe {
        found: bool,
    }
    impl<'a> syn::visit::Visit<'a> for AssertProbe {
        // statement-position macros (assert_eq!(..);) parse as Stmt::Macro
        // and never reach visit_expr_macro — both hooks must be covered
        fn visit_stmt_macro(&mut self, m: &'a syn::StmtMacro) {
            if is_assertion_macro(&m.mac.path) {
                self.found = true;
            }
        }
        fn visit_expr_macro(&mut self, m: &'a ExprMacro) {
            if is_assertion_macro(&m.mac.path) {
                self.found = true;
            }
            syn::visit::visit_expr_macro(self, m);
        }
    }
    let mut probe = AssertProbe { found: false };
    syn::visit::visit_block(&mut probe, block);
    probe.found
}

/// The test carries no assertion: `#[test]` without `#[ignore]`,
/// `#[should_panic]`, or an assert in the body.
fn is_unasserted_test(f: &ItemFn) -> bool {
    has_attr(&f.attrs, "test")
        && !has_attr(&f.attrs, "ignore")
        && !has_attr(&f.attrs, "should_panic")
        && !block_asserts(&f.block)
}

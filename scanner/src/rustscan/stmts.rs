//! Statement-level facts: whether a statement leaves the flow, a block's returned
//! values, how many functions it nests, and its line span.

use syn::visit::{self, Visit};
use syn::{Block, Expr, ItemFn, Path, Stmt};

pub(super) fn is_diverge_stmt(s: &Stmt) -> bool {
    match s {
        Stmt::Expr(e, _) => {
            matches!(e, Expr::Return(_) | Expr::Break(_) | Expr::Continue(_))
                || matches!(e, Expr::Macro(m) if is_diverging_macro(&m.mac.path))
        }
        Stmt::Macro(m) => is_diverging_macro(&m.mac.path),
        _ => false,
    }
}

pub(super) fn is_diverging_macro(path: &Path) -> bool {
    path.segments.last().is_some_and(|s| {
        matches!(
            s.ident.to_string().as_str(),
            "panic" | "unreachable" | "todo" | "unimplemented"
        )
    })
}

/// The values of every `return` in the block (closures excluded).
pub(super) fn return_values(block: &Block) -> Vec<&Expr> {
    struct Probe<'a> {
        values: Vec<&'a Expr>,
    }
    impl<'ast> Visit<'ast> for Probe<'ast> {
        fn visit_expr_return(&mut self, ret: &'ast syn::ExprReturn) {
            if let Some(value) = &ret.expr {
                self.values.push(value);
            }
        }
        fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {}
    }
    let mut probe = Probe { values: Vec::new() };
    probe.visit_block(block);
    probe.values
}

/// The latent-class counter: NESTED FN DEFINITIONS at any depth.
///
/// Deliberately NOT closures: Python's rule counts lambdas because a Python
/// lambda is a discouraged single-expression shim — a smell. A Rust closure
/// is the idiomatic local abstraction (iterators, builders, callbacks) and
/// counting it would fire on every well-structured function. A nested `fn`
/// definition, by contrast, is named structure hiding inside another
/// function in BOTH languages — that is the class-in-disguise signal.
pub(super) fn inner_count(block: &Block) -> u32 {
    struct Counter {
        count: u32,
    }
    impl<'ast> Visit<'ast> for Counter {
        fn visit_item_fn(&mut self, f: &'ast ItemFn) {
            self.count += 1;
            visit::visit_item_fn(self, f);
        }
    }
    let mut c = Counter { count: 0 };
    c.visit_block(block);
    c.count
}

pub(super) fn span_lines(start: usize, end: usize) -> usize {
    end.saturating_sub(start)
}

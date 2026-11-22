//! The one control-flow descent the loop family reads: a statement's child
//! statement lists, tagged by what the branch IS. Count, writes, bindings and
//! the walk apply their own policies to the same relation, so a new syn shape
//! is taught once.

/// What a branch IS. Only `LoopBody` changes a policy — a body local is not
/// fn-scope state, and a nested loop is its own pass. The kind also gives each
/// sibling branch its own sequence path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BranchKind {
    Then,
    Else,
    Arm,
    Block,
    LoopBody,
}

pub(super) struct Branch<'a> {
    pub(super) kind: BranchKind,
    pub(super) stmts: &'a [syn::Stmt],
}

/// The rule object for the branch relation — the one place a control-flow
/// statement's child lists are read.
pub(super) struct Branches;

impl Branches {
    /// Every statement list a control-flow statement contains, in source order.
    /// An expression-valued branch (`x => a + b`) has no statement list and
    /// contributes nothing; a block, if, match or loop body recurses.
    pub(super) fn of(stmt: &syn::Stmt) -> Vec<Branch<'_>> {
        let syn::Stmt::Expr(e, _) = stmt else {
            return Vec::new();
        };
        Self::of_expr(e)
    }

    pub(super) fn of_expr(e: &syn::Expr) -> Vec<Branch<'_>> {
        let mut out = Vec::new();
        Self::push_expr(e, &mut out);
        out
    }

    fn push_expr<'a>(e: &'a syn::Expr, out: &mut Vec<Branch<'a>>) {
        match e {
            syn::Expr::If(i) => {
                out.push(Branch {
                    kind: BranchKind::Then,
                    stmts: &i.then_branch.stmts,
                });
                if let Some((_, els)) = &i.else_branch {
                    match els.as_ref() {
                        syn::Expr::If(_) => Self::push_expr(els, out),
                        syn::Expr::Block(b) => out.push(Branch {
                            kind: BranchKind::Else,
                            stmts: &b.block.stmts,
                        }),
                        _ => {}
                    }
                }
            }
            syn::Expr::Match(m) => {
                for arm in &m.arms {
                    Self::push_arm(&arm.body, out);
                }
            }
            syn::Expr::Block(b) => out.push(Branch {
                kind: BranchKind::Block,
                stmts: &b.block.stmts,
            }),
            syn::Expr::Unsafe(u) => out.push(Branch {
                kind: BranchKind::Block,
                stmts: &u.block.stmts,
            }),
            syn::Expr::ForLoop(f) => out.push(Branch {
                kind: BranchKind::LoopBody,
                stmts: &f.body.stmts,
            }),
            syn::Expr::While(w) => out.push(Branch {
                kind: BranchKind::LoopBody,
                stmts: &w.body.stmts,
            }),
            syn::Expr::Loop(l) => out.push(Branch {
                kind: BranchKind::LoopBody,
                stmts: &l.body.stmts,
            }),
            _ => {}
        }
    }

    fn push_arm<'a>(body: &'a syn::Expr, out: &mut Vec<Branch<'a>>) {
        match body {
            syn::Expr::Block(b) => out.push(Branch {
                kind: BranchKind::Arm,
                stmts: &b.block.stmts,
            }),
            syn::Expr::If(_)
            | syn::Expr::Match(_)
            | syn::Expr::Unsafe(_)
            | syn::Expr::ForLoop(_)
            | syn::Expr::While(_)
            | syn::Expr::Loop(_) => Self::push_expr(body, out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flatten(stmt: &syn::Stmt) -> Vec<BranchKind> {
        Branches::of(stmt).into_iter().map(|b| b.kind).collect()
    }

    #[test]
    fn an_else_if_contributes_its_own_branch() {
        let stmt: syn::Stmt = syn::parse_quote! {
            if a {
                one();
            } else if b {
                two();
            } else {
                three();
            }
        };
        assert_eq!(
            flatten(&stmt),
            vec![BranchKind::Then, BranchKind::Then, BranchKind::Else]
        );
    }

    #[test]
    fn a_match_arm_loop_body_is_a_loop_branch() {
        let stmt: syn::Stmt = syn::parse_quote! {
            match n {
                0 => {}
                1 => for x in xs {
                    use_x(x);
                },
                _ => {}
            }
        };
        assert_eq!(
            flatten(&stmt),
            vec![BranchKind::Arm, BranchKind::LoopBody, BranchKind::Arm]
        );
    }

    #[test]
    fn an_expression_arm_contributes_nothing() {
        let stmt: syn::Stmt = syn::parse_quote! {
            match n {
                0 => a + b,
                _ => c,
            }
        };
        assert!(flatten(&stmt).is_empty());
    }
}

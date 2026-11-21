//! The Rust skeleton: a signature and body with names and literals abstracted
//! away, so two functions can be compared by shape.

use syn::{BinOp, Block, Expr, Item, Pat, Signature, Stmt};

/// The Rust skeleton: a signature and body with names and literals abstracted away.
pub struct RsSkeleton;

impl RsSkeleton {
    /// The structural fingerprint for the duplicate search — the Python layer's
    /// shape (node kinds with names/literals/params collapsed) over syn's AST.
    /// Token vocab is Rust-shaped (kind names + operator tokens); consistency
    /// within the pool is what the dice threshold needs, not a shared vocab.
    pub(super) fn rs_skeleton(sig: &Signature, block: &Block) -> Vec<String> {
        let mut toks = vec!["Fn".to_string()];
        for _input in &sig.inputs {
            toks.push("A".to_string()); // params collapsed, like Python's "A"
        }
        for stmt in &block.stmts {
            Self::skel_stmt(&mut toks, stmt);
        }
        toks
    }

    fn skel_stmt(toks: &mut Vec<String>, s: &Stmt) {
        match s {
            Stmt::Local(l) => {
                toks.push("Let".into());
                Self::skel_pat(toks, &l.pat);
                if let Some(init) = &l.init {
                    Self::skel_expr(toks, &init.expr);
                }
            }
            Stmt::Expr(e, _) => {
                toks.push("ExprStmt".into());
                Self::skel_expr(toks, e);
            }
            Stmt::Item(i) => match i {
                Item::Fn(f) => {
                    toks.push("Fn".into());
                    for s in &f.block.stmts {
                        Self::skel_stmt(toks, s);
                    }
                }
                Item::Struct(_) => toks.push("Struct".into()),
                Item::Enum(_) => toks.push("Enum".into()),
                Item::Impl(_) => toks.push("Impl".into()),
                Item::Use(_) => toks.push("Use".into()),
                Item::Mod(_) => toks.push("Mod".into()),
                Item::Const(_) => toks.push("Const".into()),
                Item::Static(_) => toks.push("Static".into()),
                Item::Trait(_) => toks.push("Trait".into()),
                Item::Type(_) => toks.push("Type".into()),
                _ => toks.push("Item".into()),
            },
            Stmt::Macro(_) => toks.push("MacroStmt".into()),
        }
    }

    // lucidlint: ignore large-function the skeleton mirrors syn's expr shapes — one exhaustive match is the point
    fn skel_expr(toks: &mut Vec<String>, e: &Expr) {
        match e {
            Expr::Binary(b) => {
                toks.push(Self::binop_token(&b.op).into());
                Self::skel_expr(toks, &b.left);
                Self::skel_expr(toks, &b.right);
            }
            Expr::Lit(_) => toks.push("C".into()),
            Expr::Path(_) => toks.push("N".into()),
            Expr::Call(c) => {
                toks.push("Call".into());
                Self::skel_expr(toks, &c.func);
                for a in &c.args {
                    Self::skel_expr(toks, a);
                }
            }
            Expr::MethodCall(m) => {
                toks.push("MethodCall".into());
                Self::skel_expr(toks, &m.receiver);
                for a in &m.args {
                    Self::skel_expr(toks, a);
                }
            }
            Expr::If(i) => {
                toks.push("If".into());
                Self::skel_expr(toks, &i.cond);
                for s in &i.then_branch.stmts {
                    Self::skel_stmt(toks, s);
                }
                if let Some((_, else_e)) = &i.else_branch {
                    Self::skel_expr(toks, else_e);
                }
            }
            Expr::Match(m) => {
                toks.push("Match".into());
                Self::skel_expr(toks, &m.expr);
                for arm in &m.arms {
                    toks.push("Arm".into());
                    Self::skel_pat(toks, &arm.pat);
                    Self::skel_expr(toks, &arm.body);
                }
            }
            Expr::Return(r) => {
                toks.push("Return".into());
                if let Some(e) = &r.expr {
                    Self::skel_expr(toks, e);
                }
            }
            Expr::Break(_) => toks.push("Break".into()),
            Expr::Continue(_) => toks.push("Continue".into()),
            Expr::ForLoop(f) => {
                toks.push("For".into());
                Self::skel_pat(toks, &f.pat);
                Self::skel_expr(toks, &f.expr);
                for s in &f.body.stmts {
                    Self::skel_stmt(toks, s);
                }
            }
            Expr::While(w) => {
                toks.push("While".into());
                Self::skel_expr(toks, &w.cond);
                for s in &w.body.stmts {
                    Self::skel_stmt(toks, s);
                }
            }
            Expr::Loop(l) => {
                toks.push("Loop".into());
                for s in &l.body.stmts {
                    Self::skel_stmt(toks, s);
                }
            }
            Expr::Closure(c) => {
                toks.push("Closure".into());
                Self::skel_expr(toks, &c.body);
            }
            Expr::Field(f) => {
                toks.push("Field".into());
                Self::skel_expr(toks, &f.base);
            }
            Expr::Index(i) => {
                toks.push("Index".into());
                Self::skel_expr(toks, &i.expr);
                Self::skel_expr(toks, &i.index);
            }
            Expr::Unary(u) => {
                toks.push(
                    match u.op {
                        syn::UnOp::Neg(_) => "Neg",
                        syn::UnOp::Not(_) => "Not",
                        syn::UnOp::Deref(_) => "Deref",
                        _ => "UnOpX",
                    }
                    .into(),
                );
                Self::skel_expr(toks, &u.expr);
            }
            Expr::Reference(_) => {
                toks.push("Ref".into());
            }
            Expr::Tuple(t) => {
                toks.push("Tuple".into());
                for e in &t.elems {
                    Self::skel_expr(toks, e);
                }
            }
            Expr::Array(a) => {
                toks.push("Array".into());
                for e in &a.elems {
                    Self::skel_expr(toks, e);
                }
            }
            Expr::Struct(s) => {
                toks.push("Struct".into());
                for f in &s.fields {
                    Self::skel_expr(toks, &f.expr);
                }
            }
            Expr::Cast(c) => {
                toks.push("Cast".into());
                Self::skel_expr(toks, &c.expr);
            }
            Expr::Assign(a) => {
                toks.push("Assign".into());
                Self::skel_expr(toks, &a.left);
                Self::skel_expr(toks, &a.right);
            }
            Expr::Macro(_) => toks.push("Macro".into()),
            Expr::Paren(p) => Self::skel_expr(toks, &p.expr),
            Expr::Range(_) => toks.push("Range".into()),
            Expr::Try(t) => {
                toks.push("Try".into());
                Self::skel_expr(toks, &t.expr);
            }
            Expr::Async(a) => {
                toks.push("Async".into());
                for s in &a.block.stmts {
                    Self::skel_stmt(toks, s);
                }
            }
            Expr::Await(a) => {
                toks.push("Await".into());
                Self::skel_expr(toks, &a.base);
            }
            Expr::Block(b) => {
                toks.push("Block".into());
                for s in &b.block.stmts {
                    Self::skel_stmt(toks, s);
                }
            }
            Expr::Let(l) => {
                toks.push("IfLet".into());
                Self::skel_pat(toks, &l.pat);
                Self::skel_expr(toks, &l.expr);
            }
            Expr::Yield(_) => toks.push("Yield".into()),
            Expr::Verbatim(_) => toks.push("Verbatim".into()),
            Expr::Group(g) => Self::skel_expr(toks, &g.expr),
            Expr::Infer(_) => toks.push("Infer".into()),
            Expr::Const(c) => {
                toks.push("Const".into());
                for s in &c.block.stmts {
                    Self::skel_stmt(toks, s);
                }
            }
            Expr::RawAddr(a) => {
                toks.push("RawAddr".into());
                Self::skel_expr(toks, &a.expr);
            }
            Expr::Repeat(r) => {
                toks.push("Repeat".into());
                Self::skel_expr(toks, &r.expr);
                Self::skel_expr(toks, &r.len);
            }
            Expr::TryBlock(t) => {
                toks.push("TryBlock".into());
                for s in &t.block.stmts {
                    Self::skel_stmt(toks, s);
                }
            }
            Expr::Unsafe(u) => {
                toks.push("Unsafe".into());
                for s in &u.block.stmts {
                    Self::skel_stmt(toks, s);
                }
            }
            _ => toks.push("X".into()), // syn is #[non_exhaustive] — unknown exprs collapse
        }
    }

    fn skel_pat(toks: &mut Vec<String>, p: &Pat) {
        match p {
            Pat::Ident(_) => toks.push("N".into()),
            Pat::Wild(_) => toks.push("Wild".into()),
            Pat::Lit(_) => toks.push("C".into()),
            Pat::Tuple(t) => {
                toks.push("PatTuple".into());
                for e in &t.elems {
                    Self::skel_pat(toks, e);
                }
            }
            Pat::Struct(s) => {
                toks.push("PatStruct".into());
                for f in &s.fields {
                    Self::skel_pat(toks, &f.pat);
                }
            }
            Pat::Type(t) => {
                toks.push("PatType".into());
                Self::skel_pat(toks, &t.pat);
            }
            Pat::Or(o) => {
                toks.push("PatOr".into());
                for c in &o.cases {
                    Self::skel_pat(toks, c);
                }
            }
            Pat::Slice(s) => {
                toks.push("PatSlice".into());
                for e in &s.elems {
                    Self::skel_pat(toks, e);
                }
            }
            Pat::Reference(r) => {
                toks.push("PatRef".into());
                Self::skel_pat(toks, &r.pat);
            }
            Pat::Const(_) => toks.push("PatConst".into()),
            Pat::Paren(p) => Self::skel_pat(toks, &p.pat),
            Pat::Verbatim(_) => toks.push("PatVerbatim".into()),
            Pat::Macro(_) => toks.push("PatMacro".into()),
            Pat::Rest(_) => toks.push("PatRest".into()),
            Pat::Path(_) => toks.push("N".into()),
            Pat::Range(_) => toks.push("PatRange".into()),
            Pat::TupleStruct(t) => {
                toks.push("PatTupleStruct".into());
                for e in &t.elems {
                    Self::skel_pat(toks, e);
                }
            }
            _ => toks.push("PatX".into()),
        }
    }

    fn binop_token(op: &BinOp) -> &'static str {
        use syn::BinOp::*;
        match op {
            Add(_) => "Add",
            Sub(_) => "Sub",
            Mul(_) => "Mul",
            Div(_) => "Div",
            Rem(_) => "Rem",
            And(_) => "And",
            Or(_) => "Or",
            BitXor(_) => "BitXor",
            BitAnd(_) => "BitAnd",
            BitOr(_) => "BitOr",
            Shl(_) => "Shl",
            Shr(_) => "Shr",
            Eq(_) => "Eq",
            Lt(_) => "Lt",
            Le(_) => "Le",
            Ne(_) => "Ne",
            Ge(_) => "Ge",
            Gt(_) => "Gt",
            AddAssign(_) => "AddAssign",
            SubAssign(_) => "SubAssign",
            MulAssign(_) => "MulAssign",
            DivAssign(_) => "DivAssign",
            RemAssign(_) => "RemAssign",
            BitXorAssign(_) => "BitXorAssign",
            BitAndAssign(_) => "BitAndAssign",
            BitOrAssign(_) => "BitOrAssign",
            ShlAssign(_) => "ShlAssign",
            ShrAssign(_) => "ShrAssign",
            _ => "Op",
        }
    }

    // --------------------------------------------------------------------------- module graph
}

//! The Rust arm of `unnamed-condition` and `unnamed-classification`: the nouns a
//! condition is about, and what the extraction would be called.

use syn::{BinOp, Expr, Member};

use super::RsStruct;

/// The Rust arm of `unnamed-condition`: the nouns a condition is about and the extraction's name.
pub struct RsDecision;

impl RsDecision {
    /// The operands of a boolean condition, flattening nested `&&`/`||` through
    /// `not`, parentheses and references — the same operand set the Python arm
    /// sees.
    pub(super) fn rs_bool_operands(expr: &Expr) -> Vec<&Expr> {
        fn flatten<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
            match RsDecision::unwrap_rs(e) {
                Expr::Binary(b) if matches!(b.op, BinOp::And(_) | BinOp::Or(_)) => {
                    flatten(&b.left, out);
                    flatten(&b.right, out);
                }
                other => out.push(other),
            }
        }
        let mut out = Vec::new();
        if !matches!(Self::unwrap_rs(expr), Expr::Binary(b) if matches!(b.op, BinOp::And(_) | BinOp::Or(_))) {
            return out;
        }
        flatten(expr, &mut out);
        out
    }

    pub(super) fn unwrap_rs(expr: &Expr) -> &Expr {
        match expr {
            Expr::Paren(p) => Self::unwrap_rs(&p.expr),
            Expr::Group(g) => Self::unwrap_rs(&g.expr),
            Expr::Reference(r) => Self::unwrap_rs(&r.expr),
            Expr::Unary(u) if matches!(u.op, syn::UnOp::Not(_)) => Self::unwrap_rs(&u.expr),
            _ => expr,
        }
    }

    /// The distinct nouns a condition is about — a field/method receiver, a
    /// one-segment path, a free call's first argument.
    pub(super) fn rs_condition_nouns(expr: &Expr) -> Vec<String> {
        let mut nouns: Vec<String> = Self::rs_bool_operands(expr)
            .into_iter()
            .flat_map(Self::rs_operand_nouns)
            .collect();
        nouns.sort();
        nouns.dedup();
        nouns
    }

    /// Every noun one operand is about: a comparison about both sides, arithmetic
    /// about both terms (`self.low - tol` is about the line and the tolerance).
    pub(super) fn rs_operand_nouns(expr: &Expr) -> Vec<String> {
        let mut out = Vec::new();
        match Self::unwrap_rs(expr) {
            Expr::Binary(b) => {
                out.extend(Self::rs_operand_nouns(&b.left));
                out.extend(Self::rs_operand_nouns(&b.right));
            }
            other => out.extend(Self::rs_operand_noun(other)),
        }
        out
    }

    pub(super) fn rs_operand_noun(expr: &Expr) -> Option<String> {
        match Self::unwrap_rs(expr) {
            Expr::Path(p) => match (p.path.segments.len(), p.path.segments.first()) {
                (1, Some(seg)) => Some(seg.ident.to_string()),
                _ => None, // Foo::BAR is a constant, not a noun
            },
            Expr::Field(f) => Self::rs_operand_noun(&f.base),
            Expr::MethodCall(m) => Self::rs_operand_noun(&m.receiver),
            Expr::Call(c) => match c.func.as_ref() {
                Expr::Path(p) if p.path.segments.len() == 1 => c.args.first().and_then(Self::rs_operand_noun),
                other => Self::rs_operand_noun(other),
            },
            Expr::Index(i) => Self::rs_operand_noun(&i.expr),
            Expr::Binary(b) => Self::rs_operand_noun(&b.left),
            Expr::Unary(u) => Self::rs_operand_noun(&u.expr),
            _ => None,
        }
    }

    /// The operands name something a decision can be about: at least one noun, and
    /// not `self` alone.
    pub(super) fn names_a_decision(nouns: &[String]) -> bool {
        !nouns.is_empty() && !(nouns.len() == 1 && nouns[0] == "self")
    }

    /// A noun naming a type or a constant is the vocabulary the condition is
    /// written in, not a thing it decides about.
    pub(super) fn rs_names_a_type_or_constant(structs: &[RsStruct], noun: &str) -> bool {
        noun.chars().next().is_some_and(|c| c.is_uppercase()) || structs.iter().any(|s| s.name == noun)
    }

    /// The message names the object and the action; the signature is the
    /// extraction's business, so it is never prescribed.
    pub(super) fn rs_condition_message(facts: usize, nouns: &[String], owner: Option<&str>) -> String {
        let owner = owner.unwrap_or("the type this code belongs to");
        let display: Vec<String> = nouns
            .iter()
            .map(|n| {
                if n == "self" {
                    format!("this {owner}")
                } else {
                    n.clone()
                }
            })
            .collect();
        let named: Vec<&str> = display.iter().map(String::as_str).collect();
        let subject = match named.as_slice() {
            [only] => only.to_string(),
            [a, b] => format!("{a} and {b}"),
            _ => {
                let Some((last, rest)) = named.split_last() else {
                    return String::new();
                };
                format!("{}, and {last}", rest.join(", "))
            }
        };
        if nouns.len() == 1 {
            format!(
                "{facts} facts about {subject} are joined here, and nothing states what they mean together. \
                 Name the decision as a predicate over {subject}, and call it here."
            )
        } else if nouns.iter().any(|n| n == "self") {
            format!(
                "{facts} facts about {subject} are joined here, and nothing states what they mean together. \
                 Name the decision as a method on {owner}, and call it here."
            )
        } else {
            format!(
                "{facts} facts about {subject} are joined here, and nothing states what they mean together. \
                 Name the decision as a predicate, and call it here."
            )
        }
    }

    /// A short source-free description of an expression, for the classification
    /// message's subject.
    pub(super) fn rs_expr_text(expr: &Expr) -> String {
        match Self::unwrap_rs(expr) {
            Expr::Path(p) => p
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_else(|| "the value".into()),
            Expr::Field(f) => {
                let member = match &f.member {
                    Member::Named(id) => id.to_string(),
                    Member::Unnamed(i) => i.index.to_string(),
                };
                format!("{}.{member}", Self::rs_expr_text(&f.base))
            }
            Expr::MethodCall(m) => format!("{}.{}()", Self::rs_expr_text(&m.receiver), m.method),
            Expr::Call(c) => format!("{}()", Self::rs_expr_text(&c.func)),
            Expr::Index(i) => format!("{}[...]", Self::rs_expr_text(&i.expr)),
            _ => "the value".into(),
        }
    }
}

//! `rule-table`: a rule battery's `if cond { out.push("msg") }` arms become one
//! table of (predicate, message) pairs and a collector loop.

use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Expr, FnArg, Item, ItemFn, Lit, Pat, Stmt};

use super::text::FixText;

/// `rule-table`: the rule object. Its methods read one rule battery and write the table rewrite.
pub struct RuleTable;

/// One rule battery's analyzed shape: the typed params, the accumulator name,
/// and the (condition, message) arms — the latent data structure to hoist.
pub(super) struct RuleBattery {
    params: Vec<(String, String)>,
    acc: String,
    arms: Vec<(String, String)>,
}

impl RuleTable {
    /// Does the block end with a `return ...;` statement? Only such arms are
    /// match-rewritable blind: diverging arms need no matching types.
    pub(super) fn block_ends_with_return(block: &syn::Block) -> bool {
        matches!(block.stmts.last(), Some(syn::Stmt::Expr(syn::Expr::Return(_), Some(_))))
    }

    /// Is `name` a parameter declared `&str` (no lifetime)? The match rewrite
    /// only compiles for a `&str` selector.
    pub(super) fn param_is_str(sig: &syn::Signature, name: &str) -> bool {
        sig.inputs.iter().any(|a| match a {
            syn::FnArg::Typed(pt) => {
                matches!(&*pt.pat, syn::Pat::Ident(pi) if pi.ident == name)
                    && matches!(
                        pt.ty.as_ref(),
                        syn::Type::Reference(r)
                            if r.lifetime.is_none()
                                && matches!(
                                    r.elem.as_ref(),
                                    syn::Type::Path(tp)
                                        if tp.qself.is_none() && tp.path.is_ident("str")
                                )
                    )
            }
            _ => false,
        })
    }

    /// The shape analysis of a rule battery: `let mut <acc> = vec![];`, >= 3
    /// `if <cond> { <acc>.push("msg"); }` checks (conditions read only the fn's
    /// params — fn-pointer conditions cannot capture), and a trailing
    /// `<acc>` expression. Err(why) when the body is not that shape.
    pub(super) fn rule_battery_shape(source: &str, target: &ItemFn) -> Result<RuleBattery, String> {
        let params: Vec<(String, String)> = target
            .sig
            .inputs
            .iter()
            .filter_map(|a| match a {
                FnArg::Typed(t) => match t.pat.as_ref() {
                    Pat::Ident(pi) => {
                        let ts = FixText::byte_offset(source, t.ty.span().start());
                        let te = FixText::byte_offset(source, t.ty.span().end());
                        Some((pi.ident.to_string(), source[ts..te].trim().to_string()))
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let pset: std::collections::HashSet<String> = params.iter().map(|(n, _)| n.clone()).collect();
        let stmts = &target.block.stmts;
        if stmts.len() < 2 {
            // an empty body would panic on stmts[0] — refuse cleanly
            return Err("the function body is empty".to_string());
        }
        let Stmt::Local(init) = &stmts[0] else {
            return Err("the function does not open with an accumulator binding".to_string());
        };
        let Pat::Ident(acc_pat) = &init.pat else {
            return Err("the accumulator binding is not a plain name".to_string());
        };
        let acc = acc_pat.ident.to_string();
        let mut arms: Vec<(String, String)> = Vec::new();
        let mut idx = 1usize;
        while idx < stmts.len() - 1 {
            let Stmt::Expr(e, _) = &stmts[idx] else { break };
            let Expr::If(i) = e else { break };
            let block = &i.then_branch;
            let push_stmt = block
                .stmts
                .iter()
                .find(|s| matches!(s, Stmt::Expr(se, Some(_)) if Self::is_push_call(se, &acc)))
                .ok_or_else(|| "an arm does not push to the accumulator".to_string())?;
            let Stmt::Expr(se, Some(_)) = push_stmt else {
                unreachable!()
            };
            let Expr::MethodCall(mc) = se else { unreachable!() };
            let Expr::Lit(syn::ExprLit { lit: Lit::Str(s), .. }) = &mc.args[0] else {
                return Err("an arm pushes a non-string literal".to_string());
            };
            let cond_text = FixText::span_text(source, i.cond.span())?;
            for name in Self::idents_in_expr(&i.cond) {
                if !pset.contains(&name) {
                    return Err(format!(
                        "check reads '{name}' — not a parameter (fn-pointer conditions cannot capture)"
                    ));
                }
            }
            arms.push((cond_text, s.token().to_string()));
            idx += 1;
        }
        if arms.len() < 3 {
            return Err("fewer than 3 checks — not a battery".to_string());
        }
        if idx != stmts.len() - 1 {
            // the rewrite replaces the WHOLE body (the table + collector) — any
            // statement between the checks and the trailing accumulator would be
            // silently deleted; refuse instead
            return Err("statements between the checks and the accumulator — not a pure battery".to_string());
        }
        let tail = &stmts[stmts.len() - 1];
        let Stmt::Expr(te, _) = tail else {
            return Err("the function does not end with the accumulator expression".to_string());
        };
        let Expr::Path(tp) = te else {
            return Err("the function does not end with the accumulator expression".to_string());
        };
        if tp.path.segments.len() != 1 || tp.path.segments[0].ident != acc {
            return Err("the function does not end with the accumulator expression".to_string());
        }
        Ok(RuleBattery { params, acc, arms })
    }

    pub fn fix_rule_table(source: &str, line: usize) -> Result<String, String> {
        let file = syn::parse_file(source).map_err(|_| "the file does not parse".to_string())?;
        let target = file
            .items
            .iter()
            .find_map(|item| match item {
                Item::Fn(f) if f.sig.ident.span().start().line == line => Some(f),
                _ => None,
            })
            .ok_or_else(|| format!("no function starts at line {line}"))?;
        let battery = Self::rule_battery_shape(source, target)?;
        let params = &battery.params;
        let arms = &battery.arms;
        let acc = &battery.acc;
        let param_sig: Vec<String> = params.iter().map(|(n, t)| format!("{n}: {t}")).collect();
        let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
        let args = param_names.join(", ");
        // the condition predicates BEFORE the fn — `fn _rule_N(<params>) -> bool`
        // (fn-pointer conditions: Rust closures cannot form a homogeneous table,
        // so the conditions stay pure and the table pairs them with the messages)
        let mut helpers = String::new();
        for (i, (cond, _)) in arms.iter().enumerate() {
            helpers.push_str(&format!(
                "fn _rule_{}({}) -> bool {{\n    {cond}\n}}\n\n",
                i + 1,
                param_sig.join(", ")
            ));
        }
        // the hoisted (condition, violation) table + the collector — parity with
        // the Python lambda-table (the table IS the latent data structure).
        // The header slice runs from the ITEM start (attrs + visibility + fn
        // keyword) — slicing from the signature start dropped #[cfg]/#[allow]
        // attrs and `pub` (review-log H3).
        let fn_start = FixText::byte_offset(source, target.span().start());
        let body_start = FixText::byte_offset(source, target.block.brace_token.span.open().start()) + 1;
        let body_end = FixText::byte_offset(source, target.block.brace_token.span.close().end()) - 1;
        let mut new_body = String::from("\n");
        let type_only: Vec<String> = params.iter().map(|(_, t)| t.clone()).collect();
        let fn_type = format!("fn({}) -> bool", type_only.join(", "));
        new_body.push_str(&format!(
            "    let rules: [({fn_type}, &'static str); {}] = [\n",
            arms.len()
        ));
        for (i, (_, lit)) in arms.iter().enumerate() {
            new_body.push_str(&format!("        (_rule_{i}, {lit}),\n", i = i + 1));
        }
        new_body.push_str(
            "    ];
    ",
        );
        new_body.push_str(&format!("    let mut {acc} = vec![];\n"));
        new_body.push_str(&format!(
            "    for (_cond, _msg) in rules {{\n        if _cond({args}) {{\n            {acc}.push(_msg);\n        }}\n    }}\n"
        ));
        new_body.push_str(acc);
        // the helpers go AFTER any file-level inner attributes (`#![...]`,
        // `//!` docs) — prepending before them would break the file
        let inner_end = file.attrs.iter().map(|a| a.span().end()).max();
        let insert_at = match inner_end {
            Some(e) => FixText::byte_offset(source, e),
            None => 0,
        };
        let mut out = String::new();
        out.push_str(&source[..insert_at]);
        out.push_str(&helpers);
        out.push_str(&source[insert_at..fn_start]);
        out.push_str(&source[fn_start..body_start]);
        out.push_str(&new_body);
        out.push_str(&source[body_end..]);
        Ok(out)
    }

    pub(super) fn is_push_call(e: &Expr, acc: &str) -> bool {
        match e {
            Expr::MethodCall(m) => {
                m.method == "push"
                    && matches!(m.receiver.as_ref(), Expr::Path(p)
                        if p.path.segments.len() == 1 && p.path.segments[0].ident == acc)
            }
            _ => false,
        }
    }

    /// The bare identifiers referenced in an expression.
    pub(super) fn idents_in_expr(e: &Expr) -> Vec<String> {
        struct Idents(Vec<String>);
        impl syn::visit::Visit<'_> for Idents {
            fn visit_expr_path(&mut self, node: &syn::ExprPath) {
                if node.path.segments.len() == 1 {
                    let n = node.path.segments[0].ident.to_string();
                    if !self.0.contains(&n) {
                        self.0.push(n);
                    }
                }
            }
        }
        let mut v = Idents(Vec::new());
        v.visit_expr(e);
        v.0
    }
}

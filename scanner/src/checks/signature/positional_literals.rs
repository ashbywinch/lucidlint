//! `positional-literals`: positional literals of the same kind — the classic
//! argument-swapping bug (`set_limits(10, 20)` — which is min, which is max?).

use ruff_python_ast::{Expr, ExprCall};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

use super::super::builtin_shadow::SHADOWED_BUILTINS;

/// The positional-literals rule.
pub struct PositionalLiterals;

impl PositionalLiterals {
    /// Warn on a plain call passing two or more same-kind literals.
    pub fn findings(state: &mut ScanState, call: &ExprCall, source: &str) {
        // Only PLAIN function calls (a Name callee, not a method): the builtin
        // methods that dominate real code (dict.get, str.replace, os.environ.get)
        // have canonical positional semantics — keywords are impossible or never
        // swapped, and flagging them is pure noise. User-defined functions have
        // no such convention, so a same-kind literal pair there is a real swap
        // risk. Builtin NAMES (range/print/min/max) are fixed-arities — exempt.
        let Expr::Name(n) = call.func.as_ref() else {
            return;
        };
        if SHADOWED_BUILTINS.contains(&n.id.as_str()) {
            return;
        }
        let callee_name = n.id.to_string();
        let mut ints = 0usize;
        let mut floats = 0usize;
        let mut strings = 0usize;
        for arg in &call.arguments.args {
            match arg {
                Expr::NumberLiteral(n) => match n.value {
                    ruff_python_ast::Number::Int(_) => ints += 1,
                    ruff_python_ast::Number::Float(_) => floats += 1,
                    ruff_python_ast::Number::Complex { .. } => {}
                },
                Expr::StringLiteral(_) => strings += 1,
                _ => {}
            }
        }
        let (n, kind) = if ints >= 2 {
            (ints, "numbers")
        } else if floats >= 2 {
            (floats, "floats")
        } else if strings >= 2 {
            (strings, "strings")
        } else {
            return;
        };
        let fn_name = state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        // The directive states THIS site's case — no conditional for the reader
        // to evaluate (user ruling: never suggest a fix that cannot fix). A
        // same-file callee resolves from the call's file alone, so the bare
        // command fixes; anything else carries its semantic slot up front, the
        // same contract as magic-number's --name <CONST>. A call textually
        // above its def misclassifies toward the slot — an unnecessary
        // placeholder, never a command that cannot fix.
        let directive = if state.defs.iter().any(|(name, _)| *name == callee_name) {
            " — fix: positional-literals"
        } else {
            " — fix: positional-literals --params <names>"
        };
        state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                file: state.file.to_string(),
                line: Source::line_of(source, call.range().start()),
                function: fn_name,
                kind: "positional-literals".into(),
                severity: "warn".into(),
                message: format!(
                    "call passes {n} {kind} positionally to {callee}() — a swapped argument is a silent bug; use keyword arguments{directive}",
                    callee = callee_name,
                ), });
    }
}

//! The repo-wide unused family: a definition no production code references, and a
//! setter no caller uses. Computed across every file of one invocation.

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::line_of;
use crate::ScanState;

/// `unused` / `unused-setter`: the rule objects. Their methods read the repo's production references and the setters no caller uses.
pub struct Unused;

impl Unused {
    /// Collect `set_*` methods (and property setters) for the repo-wide
    /// unused-setter pass — runs per file; reference counting needs the whole
    /// repo (prod vs test split) to distinguish dead from test-only.
    pub fn collect_setters(state: &mut ScanState, body: &[Stmt]) {
        fn walk(state: &mut ScanState, stmts: &[Stmt], source: &str) {
            for s in stmts {
                match s {
                    Stmt::FunctionDef(f) => {
                        let is_setter = f.name.as_str().starts_with("set_")
                            || f.decorator_list
                                .iter()
                                .any(|d| matches!(&d.expression, Expr::Attribute(a) if a.attr.as_str() == "setter"));
                        if is_setter {
                            state
                                .setters
                                .push((f.name.to_string(), line_of(source, f.name.range().start())));
                        }
                    }
                    Stmt::ClassDef(cd) => walk(state, &cd.body, source),
                    _ => {}
                }
            }
        }
        walk(state, body, state.source);
    }

    /// Remove Setting Method: a `set_*` method (or a property setter) referenced
    /// nowhere in production is dead — deletable, not just unused. Runs
    /// repo-wide so prod refs (same-file AND cross-file) are complete.
    ///
    /// Test-only references do NOT make it live: a test seam for code nothing
    /// ships calls is not a seam, it is dead code wearing a harness. The message
    /// names the imminent-caller escape so an agent mid-refactor is not wrongly
    /// told to delete.
    pub fn unused_setter_findings(
        setters: &[(String, String, usize)],
        prod_refs: &HashSet<String>,
        test_refs: &HashSet<String>,
    ) -> Vec<Finding> {
        let mut out = Vec::new();
        for (rel, name, line) in setters {
            if prod_refs.contains(name) {
                continue; // referenced by production — live
            }
            let test_only = test_refs.contains(name);
            let message = if test_only {
                format!(
                    "setter '{name}' ({rel}:{line}) is referenced only from tests, never from production — a test seam for dead code is not a seam: nothing shipped calls it, so it is dead. If the production caller is imminent, write the calling code — this finding then clears. Public API entry points are exempt"
                )
            } else {
                format!("setter '{name}' ({rel}:{line}) is never referenced — Remove Setting Method: delete it")
            };
            out.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                file: rel.to_string(),
                line: *line,
                col: 0,
                function: name.to_string(),
                kind: "unused-setter".into(),
                severity: "warn".into(),
                message,
            });
        }
        out
    }

    // =====================================================================
    // repo-wide families: duplicate (Dice on structural skeletons) + unused
    // (defined-but-never-referenced). These are computed in the Rust runner
    // across ALL files of one invocation — mirroring _duplicate_actions and
    // _unused_actions in lucidlint.py.
    // =====================================================================
    /// Dead-code findings (`_unused_actions`). Definitions carry an `is_class`
    /// flag — the #28 dead-class arm: a never-referenced class is as dead as a
    /// never-referenced function.
    pub fn unused_findings(
        definitions: &[(String, String, usize, bool)], // (rel, name, line, is_class)
        prod_refs: &HashSet<String>,
        test_refs: &HashSet<String>,
        strings: &[String],
    ) -> Vec<Finding> {
        let mut out = Vec::new();
        for (rel, name, line, is_class) in definitions {
            let what = if *is_class { "class" } else { "function" };
            if Self::is_live_name(name, prod_refs, strings) {
                continue;
            }
            let (message, kind) = if test_refs.contains(name) {
                (
                    format!(
                        "{what} '{name}' ({rel}:{line}) is referenced only from tests — if it is a deliberate test seam (isolation hook, fixture helper), document it; otherwise production code that nothing ships calls is dead — delete it"
                    ),
                    "unused",
                )
            } else {
                (
                    format!(
                        "{what} '{name}' ({rel}:{line}) is defined but never referenced — dead code is deleted, not kept (unless it is a CLI command or public API entry point)"
                    ),
                    "unused",
                )
            };
            out.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                col: 0,
                file: rel.clone(),
                line: *line,
                function: name.clone(),
                kind: kind.into(),
                severity: "warn".into(),
                message,
            });
        }
        out
    }

    // =====================================================================
    // latent-class partition + the test-only rule families (monkeypatch,
    // skipif, fakefs) — the last per-file Python families
    // =====================================================================
    /// The name is still used: `main`, a production reference, or a string
    /// reference (a framework's dispatch by name).
    fn is_live_name(name: &str, prod_refs: &HashSet<String>, strings: &[String]) -> bool {
        name == "main" || prod_refs.contains(name) || strings.iter().any(|s| s.contains(name))
    }
}

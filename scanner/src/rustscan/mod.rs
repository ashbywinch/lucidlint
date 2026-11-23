//! The Rust language layer — syn-based per-file scan.
//!
//! Seam: the same `Finding`/`FnCc` model as the Python layer, the same shared
//! logic from the shared modules (duplicate similarity, suppression matching, the CC
//! rule table, vague-role names) — but the AST walk is genuinely Rust-shaped
//! (`syn`), and the families whose Rust analog is weak or compiler-owned
//! (monkeypatch fixtures — Rust injects via traits; class-module naming —
//! cargo owns module structure; builtin shadowing — clippy's restriction
//! set) do not exist here while Rust-only concerns (an `#[allow]` without a
//! reason, `#[ignore]`d tests) do. `swallow` here is `let _ = <value>;` —
//! the catch-that-vanishes analog; `strewing` is free fns sharing a leading
//! struct param. `use`/`mod`
//! collection feeds the cross-file import-cycle family; the code-review-graph
//! families (high-risk, layer-mix, hotspot) degrade for Rust repos
//! until the exporter speaks Rust, and `unused` is deliberately NOT re-
//! implemented — rustc's dead_code owns that with better precision.
//!
//! Test code: `#[cfg(test)]` items and `#[test]` fn bodies are skipped by the
//! production families (their literals/decisions are noise, and Rust keeps
//! tests inline where Python files them separately) — except the rules that
//! LIVE in tests: an `#[ignore]`d test is a skipif finding, and suppressions
//! still parse from the whole file.

use crate::{finding::Finding, FnCc};

mod attrs;
mod code_in_literal;
mod comments;
mod decisions;
mod exprs;
mod fakefs;
mod graph;
mod ignored_test;
mod modshape;
mod no_assert_test;
mod probe;
mod shape;
mod skeleton;
mod state;
mod stmts;
mod test_asserts_source;
mod types;

pub(crate) use code_in_literal::code_in_literal_findings;
pub(crate) use comments::RsComments;
pub(crate) use exprs::is_compound_op;
pub(crate) use graph::ModuleGraph;
pub(crate) use ignored_test::ignored_test_findings;
pub(crate) use modshape::MixedModule;
pub(crate) use no_assert_test::no_assert_test_findings;
pub(crate) use shape::RustShape;
pub(crate) use state::RsState;
pub(crate) use test_asserts_source::test_asserts_source_findings;

/// One struct: name, declaration line, named-field count, line span.
#[derive(Clone)]
pub struct RsStruct {
    pub name: String,
    pub line: usize,
    pub fields: usize,
    pub span: usize,
}

/// The Rust scan output for one file — the FileScan-shaped parts plus the
/// module-graph data the cross-file families need.
pub struct RustScan {
    pub file_name: String,
    pub findings: Vec<Finding>,
    pub cc: Vec<FnCc>,
    pub errors: usize,
    pub skeletons: Vec<crate::skeleton::SkeletonFn>,
    /// (module name, inline?) — top-level `mod` declarations.
    pub mod_decls: Vec<(String, bool)>,
    /// Raw `use` paths ("crate::a::b", "super::x", "serde::Deserialize", ...).
    pub uses: Vec<String>,
    /// The file's shape for the module-shape rules (#107).
    pub module_shape: crate::checks::ModuleShape,
    /// The file's parsed `lucidlint: ignore` suppressions — the orchestrator
    /// needs them to suppress the repo-wide families (duplicate/unused).
    pub supps: crate::suppressions::Suppressions,
    pub supps_spent: std::collections::HashSet<(usize, String)>,
}

impl RustScan {
    pub fn file_name(&self) -> String {
        self.file_name.clone()
    }

    pub fn scan(source: &str, name: &str, repo_wide: bool) -> RustScan {
        let file = match syn::parse_file(source) {
            Ok(f) => f,
            Err(_) => {
                return RustScan {
                    supps: crate::suppressions::Suppressions::default(),
                    supps_spent: Default::default(),
                    file_name: name.to_string(),
                    findings: Vec::new(),
                    cc: Vec::new(),
                    errors: 1,
                    skeletons: Vec::new(),
                    mod_decls: Vec::new(),
                    uses: Vec::new(),
                    module_shape: Default::default(),
                };
            }
        };
        let reason_lines: std::collections::HashSet<usize> = RsComments::rs_comment_lines(source)
            .into_iter()
            .filter(|(_, t)| !t.trim_start_matches(['/', '*', ' ']).trim().is_empty())
            .flat_map(|(l, _)| [l, l + 1])
            .collect();

        let mut state = RsState {
            supps: crate::suppressions::Suppressions::default(),
            supps_spent: Default::default(),
            file: name,
            source,
            file_name: name.to_string(),
            reason_lines,
            findings: Vec::new(),
            cc: Vec::new(),
            skeletons: Vec::new(),
            mod_decls: Vec::new(),
            uses: Vec::new(),
            structs: Vec::new(),
            impls: Vec::new(),
            fn_params: Vec::new(),
            strew_candidates: Vec::new(),
            fn_stack: Vec::new(),
            current_fn: None,
            expr_stack: Vec::new(),
            in_test_code: false,
            is_test_file: crate::paths::Paths::is_test_path(name),
            repo_wide,
            current_impl: None,
            bare_body: false,
            valued_returns: 0,
            at_tail: false,
            body_block: false,
        };
        // the test-only rules run even inside #[cfg(test)] mods — a pre-pass over
        // the whole file, then the production walk skips test subtrees
        state.findings.extend(ignored_test_findings(&file, name));
        state.findings.extend(no_assert_test_findings(&file, name));
        state.findings.extend(test_asserts_source_findings(&file, name));
        state.findings.extend(code_in_literal_findings(&file, name));
        state.walk_file(&file);
        let mut scan = state.finish();
        scan.module_shape = MixedModule::module_shape(&file, source);
        MixedModule::mixed_module_into(&mut scan, &file, name);
        // G4: does the file hold magic-CANDIDATE literals anywhere — the fact
        // the stale magic-number message cites when the family fires nowhere;
        // Rust's only exemption is the position rule, so the set is empty or ["position/index"].
        let mut probe = NumericLiteralProbe { found: false };
        syn::visit::Visit::visit_file(&mut probe, &file);
        let magic_exempt_labels: Vec<&'static str> = if probe.found {
            vec!["position/index"]
        } else {
            Vec::new()
        };
        // suppressions parse from the whole file, filtering every family — the
        // shared why-less rule applies (`// lucidlint: ignore` needs a why)
        let comments = RsComments::rs_comment_lines(source);
        let supps = crate::suppressions::Suppressions::from_comments(&comments);
        // complexity findings are generated from the cc array AFTER the per-file
        // pass — honor complexity suppressions here so both paths agree, and
        // record which suppressions this retain honored so the stale check does
        // not re-flag them
        let mut pre_used = crate::suppressions::PreUsedSuppressions::default();
        if !scan.cc.is_empty() {
            scan.cc.retain(|e| {
                // record what the retention's own decision honors — the SAME
                // widened window and family-aware matching that suppressed()
                // uses (a suppression used by the cc path must not be re-flagged
                // stale by apply_suppressions_impl)
                for ln in crate::suppressions::Suppressions::window_lines(e.line) {
                    if let Some(entries) = supps.line.get(&ln) {
                        for (sig, why) in entries {
                            if crate::suppressions::Suppressions::signal_matches(sig, "complexity") && !why.is_empty() {
                                pre_used.lines.insert((ln, sig.clone()));
                            }
                        }
                    }
                }
                let line_suppressed = crate::suppressions::Suppressions::suppressed("complexity", e.line, &supps);
                let file_suppressed = supps.file.get("complexity").is_some_and(|w| !w.is_empty());
                if file_suppressed {
                    pre_used.files.insert("complexity".into());
                }
                !line_suppressed && !file_suppressed
            });
        }
        let mut supps_spent = std::collections::HashSet::new();
        let mut books = crate::suppressions::SuppressionBooks {
            pre_used: &pre_used,
            spent: &mut supps_spent,
        };

        scan.findings = crate::suppressions::Suppressions::apply(
            scan.findings,
            &comments,
            name,
            "//",
            &mut books,
            &magic_exempt_labels,
        );
        scan
    }
}

use self::probe::NumericLiteralProbe;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::rustscan::shape::RustShape;
    use crate::rustscan::skeleton::RsSkeleton;
    use syn::Item;

    fn scan(src: &str) -> Vec<Finding> {
        RustScan::scan(src, "prod_mod.rs", true).findings
    }

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing fixture {name}"))
    }

    fn scan_cc(src: &str) -> Vec<FnCc> {
        RustScan::scan(src, "prod_mod.rs", true).cc
    }

    fn has_kind(fs: &[Finding], k: &str) -> bool {
        fs.iter().any(|f| f.kind == k)
    }

    #[test]
    fn unnamed_condition_joins_facts() {
        let fs = scan(&fixture("unnamed_condition_rs.rs"));
        let hits: Vec<&Finding> = fs.iter().filter(|f| f.kind == "unnamed-condition").collect();
        assert_eq!(hits.len(), 1, "{fs:?}"); // `decide`; `joins` is the whole fn
        assert!(hits[0].message.contains("about a, b, and ready"), "{}", hits[0].message);
        // a named predicate (bare body) and a self-only condition are silent
        let ok = scan(&fixture("unnamed_condition_rs_ok.rs"));
        assert!(!has_kind(&ok, "unnamed-condition"), "{ok:?}");
    }

    #[test]
    fn comment_lines_survive_raw_and_continued_strings() {
        // a `\`-continued line and a multi-line raw string each consume a
        // source line; missing either drifts every later suppression window
        let src = include_str!("../../../tests/fixtures/code/sample_f__aff414dc.rs");
        let cs = RsComments::rs_comment_lines(src);
        let (line, text) = cs.last().expect("the tail comment");
        assert_eq!(*line, 7, "comments: {cs:?}");
        assert!(text.contains("tail comment"));
    }

    #[test]
    fn unnamed_classification_cases() {
        let fs = scan(&fixture("unnamed_classification_rs.rs"));
        let hits: Vec<&Finding> = fs.iter().filter(|f| f.kind == "unnamed-classification").collect();
        assert_eq!(hits.len(), 1, "{fs:?}"); // `describe` only
        assert!(hits[0].message.contains("3 cases of op"), "{}", hits[0].message);
        // `token` (bare body), `token_with_use` (bare body after a `use`), and
        // `explained` (the fn's only `return`) are all the function's answer
    }

    #[test]
    fn cc_if_else_if_counts_elifs_not_else() {
        let src = include_str!("../../../tests/fixtures/code/sample_f__c7fc0f57.rs");
        assert_eq!(scan_cc(src)[0].cc, 3); // if + else-if; trailing else does NOT count
    }

    #[test]
    fn cc_match_arms_minus_wildcard() {
        let src = include_str!("../../../tests/fixtures/code/sample_f__ac3f8b86.rs");
        assert_eq!(scan_cc(src)[0].cc, 3); // 2 arms + base; the wildcard does NOT count
    }

    #[test]
    fn cc_loops_boolop_assert_closure() {
        let src = include_str!("../../../tests/fixtures/code/sample_f__0c5ed441.rs");
        // for(1) if(1) &&(1) while(1) loop(1) if(1) break(0) assert(1) closure(+0) ternary(1) + base
        assert_eq!(scan_cc(src)[0].cc, 8);
    }

    #[test]
    fn cc_nested_fn_excluded_from_outer() {
        let src = include_str!("../../../tests/fixtures/code/sample_inner__07da2c69.rs");
        // radon's fn_map holds module-level fns only — the nested fn's
        // decisions count for ITS scope (closures rule) but never reach cc
        let cc = scan_cc(src);
        assert_eq!(cc.len(), 1);
        assert_eq!(cc[0].cc, 2); // outer: if(1) + base
    }

    // ------------------------------------------------------------- magic
    #[test]
    fn magic_operand_and_call_arg_found() {
        let src = "fn f() -> u32 { let x = 3 * 60; foo(300); x }\n";
        let fs = scan(src);
        assert_eq!(fs.iter().filter(|f| f.kind == "magic-number").count(), 3); // 3, 60, 300 — only 0/1/2 are trivial
    }

    #[test]
    fn magic_message_carries_no_fix_directive_for_rust() {
        // the Rust fix surface refuses magic-number (main.rs --fix handles
        // extract-method/dispatch-registry/rule-table only) — the directive
        // would send agents on a fix that cannot apply (review bot)
        let src = "fn f() -> u32 { let x = 3 * 60; x }\n";
        let fs = scan(src);
        let m: Vec<&Finding> = fs.iter().filter(|f| f.kind == "magic-number").collect();
        assert!(!m.is_empty());
        assert!(!m[0].message.contains("fix:"), "{}", m[0].message);
    }

    #[test]
    fn magic_return_value_and_const_def_not_found() {
        let src = include_str!("../../../tests/fixtures/code/sample_f__074f6491.rs");
        let fs = scan(src);
        assert!(!has_kind(&fs, "magic-number"));
    }

    #[test]
    fn magic_trivial_literals_and_suffixes_skipped() {
        let src = "fn f(x: u32) -> u32 { x * 0usize + 1 + 2 }\n";
        assert!(!has_kind(&scan(src), "magic-number"));
    }

    #[test]
    fn magic_skipped_in_test_code() {
        let src = include_str!("../../../tests/fixtures/code/sample_f__ebf9b005.rs");
        let fs = scan(src);
        assert!(!has_kind(&fs, "magic-number")); // 60 lives in test code; 2 is trivial in prod
    }

    // ------------------------------------------------------------- noop
    #[test]
    fn noop_bare_path_and_binary_flagged() {
        let src = include_str!("../../../tests/fixtures/code/sample_f__8bdb7049.rs");
        let fs = scan(src);
        assert_eq!(fs.iter().filter(|f| f.kind == "noop-statement").count(), 3);
        // x; x+y; x; — calls/macros/assigns pass
    }

    #[test]
    fn noop_if_and_match_statements_flagged() {
        let src = include_str!("../../../tests/fixtures/code/sample_f__e7e1c2f9.rs");
        assert_eq!(scan(src).iter().filter(|f| f.kind == "noop-statement").count(), 2);
    }

    // ------------------------------------------------------------- unreachable
    #[test]
    fn unreachable_after_return_and_panic() {
        let src = include_str!("../../../tests/fixtures/code/sample_f__442cc792.rs");
        let fs = scan(src);
        assert_eq!(fs.iter().filter(|f| f.kind == "unreachable").count(), 2);
    }

    #[test]
    fn unreachable_nested_fn_body_does_not_leak() {
        let src = include_str!("../../../tests/fixtures/code/sample_inner__d8821e83.rs");
        assert!(!has_kind(&scan(src), "unreachable")); // inner's return ends only inner's block
    }

    // ------------------------------------------------------------- closures -> latent class
    #[test]
    fn closures_latent_class_with_cc_gate() {
        // nested FN definitions (not closures — those are idiomatic Rust)
        let mut body = String::from(include_str!("../../../tests/fixtures/code/sample_b__4e488818.rs"));
        for i in 0..18 {
            body.push_str(&format!("    if {i} > 0 {{ a(); }} else {{ b(); }}\n"));
        }
        body.push_str("    a() + b()\n}\n");
        assert!(has_kind(&scan(&body), "closures"));
    }

    #[test]
    fn closures_closures_are_not_counted() {
        // a fn full of idiomatic local closures is NOT a latent class
        let src = include_str!("../../../tests/fixtures/code/sample_f__d7b72a59.rs");
        assert!(!has_kind(&scan(src), "closures"));
    }

    #[test]
    fn closures_thin_fn_passes() {
        let src = include_str!("../../../tests/fixtures/code/sample_b__38686f92.rs");
        assert!(!has_kind(&scan(src), "closures")); // 2 inner but cc < 15 and span < 60
    }

    // ------------------------------------------------------------- large-function
    #[test]
    fn large_function_over_120_lines() {
        let mut src = String::from(include_str!("../../../tests/fixtures/code/sample_f__000f2129.rs"));
        for i in 0..130 {
            src.push_str(&format!("    n += {i};\n"));
        }
        src.push_str("    n\n}\n");
        assert!(has_kind(&scan(&src), "large-function"));
    }

    // ------------------------------------------------------------- vague-name + record-shape
    #[test]
    fn vague_role_suffix_struct_with_weight() {
        // a role-suffixed struct with 6+ methods is load-bearing — the name hides it
        let src = include_str!("../../../tests/fixtures/code/sample_m6__d2ea271c.rs");
        assert!(has_kind(&scan(src), "vague-name"));
    }

    #[test]
    fn record_shape_boundary_finding() {
        let src = include_str!("../../../tests/fixtures/code/sample_consume__9e0a8887.rs");
        let fs = scan(src);
        assert!(has_kind(&fs, "record-shape"));
        let f = fs.iter().find(|f| f.kind == "record-shape").unwrap();
        assert!(f.message.contains("consume"));
    }

    #[test]
    fn record_shape_with_methods_passes() {
        let src = include_str!("../../../tests/fixtures/code/sample_use_big__8ec3d9e1.rs");
        assert!(!has_kind(&scan(src), "record-shape"));
    }

    // ------------------------------------------------------------- suppressions
    #[test]
    fn suppression_with_why_exempts_and_whyless_is_finding() {
        // capacity: one marker exempts ONE anchor, inner-first (schema-3
        // col) — the second literal on the line stays visible until it is
        // named or gets its own marker (peel ruling)
        let src = include_str!("../../../tests/fixtures/code/sample_f__9182b618.rs");
        let fs = scan(src);
        let mn: Vec<&Finding> = fs.iter().filter(|x| x.kind == "magic-number").collect();
        assert_eq!(mn.len(), 1, "{fs:?}");
        assert!(
            mn[0].message.contains("magic number 3"),
            "the lower-col literal remains: {mn:?}"
        );
        let src2 = include_str!("../../../tests/fixtures/code/sample_f__0a97453f.rs");
        assert!(has_kind(&scan(src2), "suppression"));
    }

    #[test]
    fn allow_without_reason_is_finding() {
        let src = include_str!("../../../tests/fixtures/code/sample_f__60656ae7.rs");
        assert!(has_kind(&scan(src), "allow-reason"));
    }

    #[test]
    fn allow_with_two_line_reason_passes() {
        // the checks.rs case: a reason comment spanning two lines above the attr
        let src = include_str!("../../../tests/fixtures/code/sample_f__babfd97c.rs");
        let fs = scan(src);
        assert!(!has_kind(&fs, "allow-reason"), "{:?}", fs);
    }

    #[test]
    fn allow_with_reason_passes() {
        let src = include_str!("../../../tests/fixtures/code/sample_f__1e2ac427.rs");
        let fs = scan(src);
        assert!(!has_kind(&fs, "allow-reason"));
    }

    // ------------------------------------------------------------- test rules
    #[test]
    fn ignored_test_is_skipif_finding() {
        let src = include_str!("../../../tests/fixtures/code/sample_fast__fe07c6f6.rs");
        let fs = scan(src);
        assert!(has_kind(&fs, "skipif"));
        assert_eq!(fs.iter().filter(|f| f.kind == "skipif").count(), 1);
    }

    // ------------------------------------------------------------- comments
    #[test]
    fn comment_extraction_handles_hashed_raw_strings() {
        // r#"..."# content may contain " and // — neither is a comment
        let src = include_str!(
            "../../../tests/fixtures/code/sample_comment_extraction_handles_hashed_raw_strings__8f3e8eec.rs"
        );
        let lines = RsComments::rs_comment_lines(src);
        let texts: Vec<String> = lines.iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(texts.len(), 1);
        assert!(texts[0].contains("real comment"));
    }

    #[test]
    fn comment_extraction_is_string_aware() {
        let src = include_str!("../../../tests/fixtures/code/sample_comment_extraction_is_string_aware__75dc422d.rs");
        let lines = RsComments::rs_comment_lines(src);
        let texts: Vec<String> = lines.iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(texts.len(), 3); // the quoted "// not a comment" is not one
        assert!(texts.iter().any(|t| t.contains("real comment")));
        assert!(texts.iter().any(|t| t.contains("trailing")));
        assert!(texts.iter().any(|t| t.contains("block")));
        assert!(!texts.iter().any(|t| t.contains("not a comment")));
    }

    // ------------------------------------------------------------- duplicates
    #[test]
    fn duplicate_detection_across_rust_fns() {
        let a = include_str!("../../../tests/fixtures/code/sample_alpha__6fffef7b.rs");
        let b = include_str!("../../../tests/fixtures/code/sample_beta__b5c5f7cb.rs");
        let sa = RustScan::scan(a, "a.rs", true);
        let sb = RustScan::scan(b, "b.rs", true);
        let mut pool: Vec<crate::skeleton::SkeletonFn> = Vec::new();
        pool.extend(sa.skeletons.iter().map(|s| crate::skeleton::SkeletonFn {
            rel: "a.rs".into(),
            name: s.name.clone(),
            line: s.line,
            end_line: s.end_line,
            skeleton: s.skeleton.clone(),
        }));
        pool.extend(sb.skeletons.iter().map(|s| crate::skeleton::SkeletonFn {
            rel: "b.rs".into(),
            name: s.name.clone(),
            line: s.line,
            end_line: s.end_line,
            skeleton: s.skeleton.clone(),
        }));
        let dups = crate::checks::duplicate_findings(&pool);
        assert_eq!(dups.len(), 1, "{dups:?}");
        assert!(dups[0].message.contains("beta"));
    }

    // ------------------------------------------------------------- module graph
    #[test]
    fn module_graph_finds_import_cycle() {
        let files = vec!["main.rs".to_string(), "a.rs".to_string(), "b.rs".to_string()];
        let mut mods: HashMap<String, Vec<(String, bool)>> = HashMap::new();
        mods.insert("main.rs".into(), vec![("a".into(), false), ("b".into(), false)]);
        mods.insert("a.rs".into(), vec![]);
        mods.insert("b.rs".into(), vec![]);
        let mut uses: HashMap<String, Vec<String>> = HashMap::new();
        uses.insert("a.rs".into(), vec!["crate::b::helper".into()]);
        uses.insert("b.rs".into(), vec!["crate::a::helper".into()]);
        let graph = ModuleGraph::module_graph(&files, &mods, &uses);
        assert_eq!(graph.get("a.rs").map(|v| v.as_slice()), Some(&["b.rs".to_string()][..]));
        assert_eq!(graph.get("b.rs").map(|v| v.as_slice()), Some(&["a.rs".to_string()][..]));
        let fs = crate::graph_families::Cycles::findings_for(&graph);
        assert_eq!(fs.len(), 1);
        assert!(fs[0].message.contains("a.rs"));
        assert!(fs[0].message.contains("b.rs"));
    }

    #[test]
    fn module_graph_skips_external_crates() {
        let files = vec!["main.rs".to_string(), "a.rs".to_string()];
        let mut mods: HashMap<String, Vec<(String, bool)>> = HashMap::new();
        mods.insert("main.rs".into(), vec![("a".into(), false)]);
        mods.insert("a.rs".into(), vec![]);
        let mut uses: HashMap<String, Vec<String>> = HashMap::new();
        uses.insert("a.rs".into(), vec!["serde::Deserialize".into()]);
        let graph = ModuleGraph::module_graph(&files, &mods, &uses);
        assert!(graph.values().all(|v| v.is_empty())); // serde is not a local module — no edges
    }

    #[test]
    fn swallow_let_underscore_discards_calls() {
        let fs = scan(include_str!("../../../tests/fixtures/code/sample_f__7103100e.rs"));
        assert!(has_kind(&fs, "swallow"));
        let fs2 = scan(include_str!("../../../tests/fixtures/code/sample_f__d58838e3.rs"));
        assert!(!has_kind(&fs2, "swallow")); // plain-path moves are not swallows
    }

    #[test]
    fn swallow_skipped_in_test_code() {
        let fs = scan(include_str!("../../../tests/fixtures/code/sample_f__c5f9cae3.rs"));
        assert!(!has_kind(&fs, "swallow"));
    }

    #[test]
    fn debug_artifact_dbg_and_unwrap() {
        let fs = scan(include_str!("../../../tests/fixtures/code/sample_f__067f5aad.rs"));
        assert!(has_kind(&fs, "debug-artifact"));
        let fs2 = scan(include_str!("../../../tests/fixtures/code/sample_f__659c79d2.rs"));
        assert!(!has_kind(&fs2, "debug-artifact"));
    }

    #[test]
    fn debug_artifact_statement_position_dbg_alone() {
        // the pre-fix walker only saw expression-position macros — the
        // common `dbg!(x);` statement slipped through (found while proving
        // the LSP install; the old fixture passed on the unwrap alone)
        let fs = scan(include_str!("../../../tests/fixtures/code/sample_f__cb0c937f.rs"));
        assert!(has_kind(&fs, "debug-artifact"));
        let ok = scan(include_str!("../../../tests/fixtures/code/sample_f__772d6dd3.rs"));
        assert!(!has_kind(&ok, "debug-artifact"));
    }

    #[test]
    fn debug_artifact_skipped_in_tests() {
        let fs = scan(include_str!("../../../tests/fixtures/code/sample_f__c89d239d.rs"));
        assert!(!has_kind(&fs, "debug-artifact"));
    }

    #[test]
    fn boolean_literal_argument_is_found() {
        let fs = scan(include_str!("../../../tests/fixtures/code/sample_f__591e79b9.rs"));
        assert!(has_kind(&fs, "boolean-arg"));
        let fs2 = scan(include_str!("../../../tests/fixtures/code/sample_f__6a224272.rs"));
        assert!(!has_kind(&fs2, "boolean-arg"));
    }

    #[test]
    fn long_parameter_list_over_five() {
        // a REAL body — an empty/trivial stub is a protocol placeholder,
        // not a param-list smell (review: redirect_request override)
        let fs = scan(include_str!("../../../tests/fixtures/code/sample_f__7c67583a.rs"));
        assert!(has_kind(&fs, "long-param-list"));
        let fs2 = scan(include_str!("../../../tests/fixtures/code/sample_f__7bf4e9e3.rs"));
        assert!(!has_kind(&fs2, "long-param-list"));
    }

    #[test]
    fn long_parameter_list_skips_trivial_stub() {
        // parity with the Python rule: a 6-param override stub (like
        // urllib's redirect_request) must not be flagged — a parameter
        // object would break the framework protocol
        let stub = scan(include_str!(
            "../../../tests/fixtures/code/sample_redirect_request_stub.rs"
        ));
        assert!(!has_kind(&stub, "long-param-list"), "{:?}", stub);
        let pass = scan(include_str!("../../../tests/fixtures/code/sample_stub__dd8c4e5c.rs"));
        assert!(!has_kind(&pass, "long-param-list"), "{:?}", pass);
    }

    #[test]
    fn rust_fn_shape_classifies_dispatch_and_rules() {
        // parity with python_fn_shape: the complexity message routes by the
        // body's shape — dispatch chains and rule batteries get the lucid
        // refactoring for their shape
        let dispatch = include_str!("../../../tests/fixtures/code/sample_route__c2fe1561.rs");
        let file = syn::parse_file(dispatch).unwrap();
        let (body, sig) = match &file.items[0] {
            syn::Item::Fn(f) => (f.block.stmts.clone(), f.sig.clone()),
            _ => panic!("not a fn"),
        };
        let (shape, detail) = RustShape::fn_shape(&body, &sig);
        assert_eq!((shape, detail.as_str()), ("dispatch", "sel"));
        let rules = include_str!("../../../tests/fixtures/code/sample_check__917c07e7.rs");
        let file = syn::parse_file(rules).unwrap();
        let (body, sig2) = match &file.items[0] {
            syn::Item::Fn(f) => (f.block.stmts.clone(), f.sig.clone()),
            _ => panic!("not a fn"),
        };
        let (shape, detail) = RustShape::fn_shape(&body, &sig2);
        assert_eq!((shape, detail.as_str()), ("rules", "out"));
        let plain = "fn f(a: i32) -> i32 { a + 1 }\n";
        let file = syn::parse_file(plain).unwrap();
        let (body, sig3) = match &file.items[0] {
            syn::Item::Fn(f) => (f.block.stmts.clone(), f.sig.clone()),
            _ => panic!("not a fn"),
        };
        let (shape, _) = RustShape::fn_shape(&body, &sig3);
        assert_eq!(shape, "plain");
    }

    #[test]
    fn no_assert_test_is_flagged() {
        let fs = scan(include_str!("../../../tests/fixtures/code/sample_t__f0bef57f.rs"));
        assert!(has_kind(&fs, "no-assert-test"));
        let ok = scan(include_str!("../../../tests/fixtures/code/sample_t__80da3d6c.rs"));
        assert!(!has_kind(&ok, "no-assert-test"));
        let panic = scan(include_str!("../../../tests/fixtures/code/sample_t__72494f09.rs"));
        assert!(!has_kind(&panic, "no-assert-test"));
        let ignored = scan(include_str!("../../../tests/fixtures/code/sample_t__51b9ea1d.rs"));
        assert!(!has_kind(&ignored, "no-assert-test"));
    }

    #[test]
    fn test_asserts_source_flags_code_text_assertions() {
        let fs = scan(&fixture("test_asserts_source_bad.rs"));
        let hits: Vec<&Finding> = fs.iter().filter(|f| f.kind == "test-asserts-source").collect();
        assert_eq!(hits.len(), 5, "{fs:?}");
        let functions: Vec<&str> = hits.iter().map(|hit| hit.function.as_str()).collect();
        assert_eq!(
            functions,
            vec![
                "needle_is_code",
                "output_text_is_code",
                "code_text_variable_prose_needle",
                "code_literal_comparison",
                "two_code_literals",
            ]
        );
        assert_eq!(
            hits[0].message,
            "test 'needle_is_code' asserts on code text, not on the behaviour under test — a text match pins one spelling, so it passes even when the code does nothing. Keep the test: assert what the code actually does instead. Do not delete the test or weaken it to match the text."
        );
    }

    #[test]
    fn test_asserts_source_allows_assertions_on_the_parsed_finding() {
        let fs = scan(&fixture("test_asserts_source_ok.rs"));
        assert!(!has_kind(&fs, "test-asserts-source"), "{fs:?}");
    }

    #[test]
    fn code_in_literal_flags_a_code_block_and_a_scan_snippet() {
        let fs = scan(&fixture("code_in_literal_bad.rs"));
        let hits: Vec<&Finding> = fs.iter().filter(|f| f.kind == "code-in-literal").collect();
        assert_eq!(hits.len(), 3, "{fs:?}");
        assert!(hits
            .iter()
            .all(|h| h.message.contains("move it to a real file under tests/fixtures/")));
    }

    #[test]
    fn code_in_literal_excludes_include_str_and_messages() {
        let fs = scan(&fixture("code_in_literal_ok.rs"));
        assert!(!has_kind(&fs, "code-in-literal"), "{fs:?}");
    }

    #[test]
    fn mixed_module_fires_when_groups_do_not_meet() {
        // three pairs that reference each other and nothing else, plus four
        // stand-alone fns: 10 definitions, 3 groups -> several modules
        let src =
            include_str!("../../../tests/fixtures/code/sample_mixed_module_fires_when_groups_do_not_meet__946ea264.rs");
        let fs = scan(src);
        let hit = fs.iter().find(|f| f.kind == "mixed-module").expect("mixed-module");
        assert!(hit.message.contains("3 groups"), "{}", hit.message);
        assert!(hit.message.contains("4 items standing alone"), "{}", hit.message);
        assert!(hit.message.contains("A and B"), "{}", hit.message);
    }

    #[test]
    fn mixed_module_passes_one_connected_module() {
        // a chain references through every item: one group, however many items
        let src =
            include_str!("../../../tests/fixtures/code/sample_mixed_module_passes_one_connected_module__3804045e.rs");
        assert!(!has_kind(&scan(src), "mixed-module"));
    }

    #[test]
    fn mixed_module_passes_independent_helpers() {
        // a module of stand-alone items is one concept whose items happen not
        // to call each other: no groups, no finding
        let mut src = String::new();
        for i in 0..12 {
            src.push_str(&format!("fn helper_{i}(value: u32) -> u32 {{\n    value + {i}\n}}\n\n"));
        }
        assert!(!has_kind(&scan(&src), "mixed-module"));
    }

    #[test]
    fn mixed_module_needs_three_groups() {
        // two groups is a pair of concepts, under the bar
        let src = include_str!("../../../tests/fixtures/code/sample_mixed_module_needs_three_groups__cd5761c9.rs");
        assert!(!has_kind(&scan(src), "mixed-module"));
    }

    #[test]
    fn mixed_module_ignores_test_items() {
        // a test module references everything; the rule reads production items
        let src = include_str!("../../../tests/fixtures/code/sample_mixed_module_ignores_test_items__997505d4.rs");
        assert!(!has_kind(&scan(src), "mixed-module"));
    }

    #[test]
    fn strewing_free_fns_sharing_struct_param() {
        let fs = scan(include_str!("../../../tests/fixtures/code/sample_c__c120e836.rs"));
        assert!(has_kind(&fs, "strewing"));
        // the struct already names the data: the action is an impl, not a new class
        let msg = fs.iter().find(|f| f.kind == "strewing").unwrap().message.clone();
        assert!(msg.contains("Give 'S' an impl"), "{msg}");
        assert!(!msg.contains("extract a class"), "{msg}");
        let fs2 = scan(include_str!("../../../tests/fixtures/code/sample_b__8165808e.rs"));
        assert!(!has_kind(&fs2, "strewing"));
    }

    #[test]
    fn global_state_statics_flagged() {
        let fs = scan(include_str!(
            "../../../tests/fixtures/code/sample_global_state_statics_flagged__add8c226.rs"
        ));
        assert!(has_kind(&fs, "global-state"));
        let fs2 = scan(include_str!(
            "../../../tests/fixtures/code/sample_global_state_statics_flagged__9463cb58.rs"
        ));
        assert!(has_kind(&fs2, "global-state"));
        let ok = scan(include_str!(
            "../../../tests/fixtures/code/sample_global_state_statics_flagged__927e7bfb.rs"
        ));
        assert!(!has_kind(&ok, "global-state"));
    }

    #[test]
    fn fakefs_literal_path_in_test() {
        let fs = scan(include_str!("../../../tests/fixtures/code/sample_t__e59158e5.rs"));
        assert!(has_kind(&fs, "fakefs"));
        let ok = scan(include_str!("../../../tests/fixtures/code/sample_t__22ce9bea.rs"));
        assert!(!has_kind(&ok, "fakefs"));
        let prod = scan(include_str!("../../../tests/fixtures/code/sample_f__e347b357.rs"));
        assert!(!has_kind(&prod, "fakefs")); // test-only family
    }

    #[test]
    fn stale_suppression_flagged() {
        let fs = scan(include_str!("../../../tests/fixtures/code/sample_f__70df61b2.rs"));
        assert!(has_kind(&fs, "stale-suppression"));
        let ok = scan(include_str!("../../../tests/fixtures/code/sample_f__b0d4a305.rs"));
        assert!(!has_kind(&ok, "stale-suppression"));
    }

    #[test]
    fn detached_method_rust_flagged() {
        let fs = scan(include_str!("../../../tests/fixtures/code/sample_m__78e6a9b7.rs"));
        assert!(has_kind(&fs, "detached-method"));
        let ok = scan(include_str!("../../../tests/fixtures/code/sample_m__6d992314.rs"));
        assert!(!has_kind(&ok, "detached-method"));
        let assoc = scan(include_str!("../../../tests/fixtures/code/sample_m__4c75c78d.rs"));
        assert!(!has_kind(&assoc, "detached-method")); // no receiver — associated fn
    }

    #[test]
    fn detached_method_self_inside_macro_counts() {
        // write! hides `self` in a macro token group — the probe must descend
        let fs = scan(include_str!("../../../tests/fixtures/code/sample_fmt__f8b3953d.rs"));
        assert!(!has_kind(&fs, "detached-method"));
    }

    #[test]
    fn mixed_module_names_the_part_it_counts_when_every_item_is_an_island() {
        // 21 unrelated free functions: the fragmentation arm fires with no
        // groups at all. The message must not say "the groups are ." — it
        // states the shape it measured, and says there are no families.
        let mut src = String::new();
        for i in 0..21 {
            src.push_str(&format!("pub fn step_{i}(x: u32) -> u32 {{ x }}\n"));
        }
        let fs = scan(&src);
        let hit = fs
            .iter()
            .find(|f| f.kind == "mixed-module")
            .expect("mixed-module fires");
        assert!(!hit.message.contains("groups are ."), "{}", hit.message);
        assert!(
            hit.message.contains("None of them references another: 21 items"),
            "{}",
            hit.message
        );
        assert!(hit.message.contains("names no families at all"), "{}", hit.message);
    }

    #[test]
    fn smart_pointer_parameters_join_their_inner_type_group() {
        // a Box/Rc/Arc/Cow parameter is carried allocation, not a foreign
        // receiver: `fn f(s: Box<State>)` is a State method in exile, exactly
        // as `fn f(s: &State)` is
        let src = include_str!("../../../tests/fixtures/code/sample_d__e0ff1d3b.rs");
        let file = syn::parse_file(src).unwrap();
        let shape = crate::rustscan::modshape::MixedModule::module_shape(&file, src);
        // the three State takers are State's methods in exile, not free fns
        assert_eq!(shape.functions, 0);
        // and a pointer to a foreign type still joins that type's group
        assert_eq!(shape.receiver_groups, vec![("Expr".to_string(), 1)]);
    }

    #[test]
    fn rust_skeleton_shapes() {
        let src = "fn f(x: u32) -> u32 { if x > 0 { x + 1 } else { x } }\n";
        let file = syn::parse_file(src).unwrap();
        let Item::Fn(f) = &file.items[0] else {
            panic!("expected fn")
        };
        let skel = RsSkeleton::rs_skeleton(&f.sig, &f.block);
        assert_eq!(skel[0], "Fn");
        assert_eq!(skel[1], "A");
        assert!(skel.contains(&"If".to_string()));
        assert!(skel.contains(&"Gt".to_string()));
        assert!(skel.contains(&"C".to_string()));
        assert!(skel.contains(&"N".to_string()));
    }

    // ------------------------------------------------------------- the loop family
    #[test]
    fn loop_pipeline_push_only_body() {
        let fs = scan(&fixture("loop_pipeline_rs.rs"));
        assert!(has_kind(&fs, "loop-pipeline"), "{fs:?}");
        let f = fs.iter().find(|x| x.kind == "loop-pipeline").unwrap();
        assert!(f.message.contains(".map("), "{}", f.message);
        assert!(f.message.contains(".extend("), "{}", f.message);
        // the fixture IS the fixer contract — the directive must be present
        assert!(f.message.contains("fix: loop-pipeline"), "{}", f.message);
    }

    #[test]
    fn loop_pipeline_message_names_the_extend_case() {
        // issue #109: a push onto a collection that already existed is not a
        // `.map().collect()` build, and `.for_each()` is never the better
        // shape — the message must name `.extend()` for that case
        let src = include_str!("../../../tests/fixtures/code/sample_add_children__d610bdb0.rs");
        let fs = scan(src);
        let f = fs.iter().find(|x| x.kind == "loop-pipeline").expect("pipeline fires");
        assert!(f.message.contains(".extend("), "{}", f.message);
        assert!(!f.message.contains("for_each"), "{}", f.message);
    }

    #[test]
    fn loop_pipeline_indexed_store_gets_its_own_action() {
        // an `acc[i] = v` store is a map over the indices — `.extend()` joins
        // items and does not describe it
        let src = include_str!("../../../tests/fixtures/code/sample_scaled__958026e1.rs");
        let fs = scan(src);
        let f = fs.iter().find(|x| x.kind == "loop-pipeline").expect("pipeline fires");
        assert!(f.message.contains("over the indices"), "{}", f.message);
        assert!(!f.message.contains(".extend("), "{}", f.message);
    }

    #[test]
    fn loop_worklist_drain_is_not_a_shape_verdict() {
        // issue #109: `while let Some(x) = work.pop()` consumes the loop's own
        // worklist — the body's writes are traversal state, not outputs, so
        // the multi-output and per-item recipes do not describe the loop. The
        // push loop inside it is still a plain collection build.
        let fs = scan(&fixture("worklist_walk_rs.rs"));
        assert!(!has_kind(&fs, "mutating-loop"), "{fs:?}");
        assert!(!has_kind(&fs, "loop-hoist"), "{fs:?}");
        let pipes: Vec<&Finding> = fs.iter().filter(|f| f.kind == "loop-pipeline").collect();
        assert_eq!(pipes.len(), 1, "only the inner push loop is a build: {fs:?}");
        assert_eq!(pipes[0].line, 9, "{fs:?}");
    }

    #[test]
    fn loop_worklist_drain_with_a_separate_result_stays_silent() {
        // a drain whose body only accumulates a result is still the traversal:
        // the hoist recipe's "then combine or fold" describes neither shape
        let src = include_str!("../../../tests/fixtures/code/sample_collect__81124fe1.rs");
        let fs = scan(src);
        assert!(!has_kind(&fs, "loop-hoist"), "{fs:?}");
        assert!(!has_kind(&fs, "mutating-loop"), "{fs:?}");
    }

    #[test]
    fn loop_worklist_drain_reads_the_ufcs_spelling_too() {
        // `Vec::pop(&mut stack)` is the same drain written as a free call —
        // the removed-from name is the first argument
        let src = include_str!("../../../tests/fixtures/code/sample_drain__746e1028.rs");
        let fs = scan(src);
        assert!(!has_kind(&fs, "mutating-loop"), "{fs:?}");
        assert!(!has_kind(&fs, "loop-hoist"), "{fs:?}");
        // a shared borrow removes nothing — the two outputs still count
        let shared = include_str!("../../../tests/fixtures/code/sample_total__c75850f2.rs");
        assert!(has_kind(&scan(shared), "mutating-loop"), "{shared}");
    }

    #[test]
    fn loop_hoist_sees_a_chained_receiver_write() {
        // issue #113: `line.entry(k).or_default().push(v)` writes `line`
        // through a method-call chain. `split` has a second map write and is
        // multi-output; `fill` has only the chain and is a one-output hoist.
        let fs = scan(&fixture("loop_chained_receiver_write.rs"));
        let mut findings: Vec<(&str, &str)> = fs
            .iter()
            .filter(|f| matches!(f.kind.as_str(), "loop-hoist" | "mutating-loop"))
            .map(|f| (f.function.as_str(), f.kind.as_str()))
            .collect();
        findings.sort();
        assert_eq!(
            findings,
            vec![("fill", "loop-hoist"), ("split", "mutating-loop")],
            "{fs:?}"
        );
    }

    #[test]
    fn loop_sequence_ignores_sibling_branches() {
        // the finding set is the user outcome: only the same-block pair is a
        // sequence; the if/else and else-if arms contribute nothing.
        let fs = scan(&fixture("loop_sequence_sibling_branches.rs"));
        let sequence: Vec<(&str, &str)> = fs
            .iter()
            .filter(|f| f.kind == "loop-sequence")
            .map(|f| (f.function.as_str(), f.kind.as_str()))
            .collect();
        assert_eq!(sequence, vec![("same_block", "loop-sequence")], "{fs:?}");
        let finding = fs.iter().find(|f| f.kind == "loop-sequence").expect("sequence");
        assert!(finding.message.contains("2 sequential loops"), "{}", finding.message);
    }

    #[test]
    fn loop_growing_cursor_is_a_traversal() {
        // the finding set is the user outcome: the three traversals produce
        // nothing; the no-growth cursor is the one stateful loop.
        let fs = scan(&fixture("loop_growing_cursor.rs"));
        let mut stateful: Vec<(&str, &str)> = fs
            .iter()
            .filter(|f| matches!(f.kind.as_str(), "loop-hoist" | "mutating-loop" | "loop-pipeline"))
            .map(|f| (f.function.as_str(), f.kind.as_str()))
            .collect();
        stateful.sort();
        assert_eq!(stateful, vec![("no_growth", "mutating-loop")], "{fs:?}");
        let finding = fs.iter().find(|f| f.kind == "mutating-loop").expect("stateful");
        assert!(finding.message.contains("loop mutates"), "{}", finding.message);
    }

    #[test]
    fn loop_pipeline_requires_an_accumulator_independent_item_and_guard() {
        // the finding set is the user outcome: the five accumulator-reading
        // loops produce nothing; the two independent calls do.
        let fs = scan(&fixture("loop_pipeline_accumulator_reads.rs"));
        let mut pipeline: Vec<(&str, &str)> = fs
            .iter()
            .filter(|f| f.kind == "loop-pipeline")
            .map(|f| (f.function.as_str(), f.kind.as_str()))
            .collect();
        pipeline.sort();
        assert_eq!(
            pipeline,
            vec![
                ("call_reads_other", "loop-pipeline"),
                ("guard_reads_other", "loop-pipeline"),
            ],
            "{fs:?}"
        );
    }

    #[test]
    fn loop_pipeline_idiomatic_combinator_passes() {
        let fs = scan(&fixture("loop_pipeline_rs_ok.rs"));
        assert!(!has_kind(&fs, "loop-pipeline"), "{fs:?}");
        assert!(!has_kind(&fs, "mutating-loop"), "{fs:?}");
    }

    #[test]
    fn mutating_loop_two_surviving_writes() {
        let fs = scan(&fixture("mutating_loop_rs.rs"));
        assert!(has_kind(&fs, "mutating-loop"), "{fs:?}");
    }

    #[test]
    fn loop_hoist_long_single_mutation_body() {
        let fs = scan(&fixture("loop_hoist_rs.rs"));
        assert!(has_kind(&fs, "loop-hoist"), "{fs:?}");
        assert!(!has_kind(&fs, "mutating-loop"), "{fs:?}");
    }

    #[test]
    fn loop_hoist_directive_gated_on_fixability() {
        // the fix fixture hoists (typed accumulator) — directive on;
        // an untyped accumulator refuses the helper signature — finding
        // without the directive, never an offered fix that cannot apply
        let fs = scan(&fixture("loop_hoist_rs_fix.rs"));
        let h = fs.iter().find(|x| x.kind == "loop-hoist").expect("hoist fires");
        assert!(h.message.contains("fix: loop-hoist"), "{}", h.message);
        let untyped = scan(include_str!("../../../tests/fixtures/code/sample_score__f3b94d19.rs"));
        let u = untyped
            .iter()
            .find(|x| x.kind == "loop-hoist")
            .expect("hoist still fires");
        assert!(!u.message.contains("fix:"), "{}", u.message);
    }

    #[test]
    fn loop_sequence_shared_state_is_pipeline() {
        // the second pass reads what the first wrote — a feed chain
        let fs = scan(&fixture("loop_sequence_rs.rs"));
        assert!(has_kind(&fs, "loop-sequence"), "{fs:?}");
        let s = fs.iter().find(|x| x.kind == "loop-sequence").unwrap();
        assert!(s.message.contains("pipeline"), "{}", s.message);
    }

    #[test]
    fn loop_sequence_directive_gated_on_chain_fixability() {
        // the fix fixture chains (shared accumulator) — directive on;
        // the detector fixture feeds across accumulators — finding
        // without the directive, never an offered fix that cannot apply
        let fs = scan(&fixture("loop_sequence_rs_fix.rs"));
        let s = fs.iter().find(|x| x.kind == "loop-sequence").expect("sequence fires");
        assert!(s.message.contains("fix: loop-sequence"), "{}", s.message);
        let fed = scan(&fixture("loop_sequence_rs.rs"));
        let f = fed
            .iter()
            .find(|x| x.kind == "loop-sequence")
            .expect("feed chain fires");
        assert!(!f.message.contains("fix:"), "{}", f.message);
    }

    #[test]
    fn every_advertised_loop_fix_applies() {
        // issue #117: a finding that names `— fix: <kind>` is a promise; the
        // fixer must accept the same site (offer equals fix).
        let pipeline = include_str!("../../../tests/fixtures/code/sample_collect__83d4b5f6.rs");
        let cases = [
            (pipeline, "loop-pipeline"),
            (
                include_str!("../../../tests/fixtures/loop_hoist_rs_fix.rs"),
                "loop-hoist",
            ),
            (
                include_str!("../../../tests/fixtures/loop_sequence_rs_fix.rs"),
                "loop-sequence",
            ),
        ];
        for (src, kind) in cases {
            let findings = scan(src);
            let advertised: Vec<&Finding> = findings
                .iter()
                .filter(|f| f.kind == kind && f.message.contains(" — fix: "))
                .collect();
            assert!(
                !advertised.is_empty(),
                "the fixture must advertise {kind}: {findings:?}"
            );
            for f in advertised {
                let applies = match kind {
                    "loop-pipeline" => crate::fix::LoopPipeline::fixable(src, "x.rs", f.line),
                    "loop-hoist" => crate::fix::LoopHoist::fixable_at(src, "x.rs", f.line),
                    "loop-sequence" => crate::fix::loop_sequence_fixable(src, "x.rs", f.line),
                    _ => unreachable!(),
                };
                assert!(applies, "{} at line {} advertises a fix it refuses", kind, f.line);
            }
        }
    }

    #[test]
    fn loop_self_feeding_for_is_not_a_pipeline() {
        // a loop that reads the collection it rebuilds is a traversal, not a
        // build — the same rule as the Python side (#120). The filter is keyed
        // on the TARGET name, not any field read, so a loop over another
        // collection's items is still a pipeline.
        let fs = scan(&fixture("loop_self_feeding_for.rs"));
        let pipes: Vec<&Finding> = fs.iter().filter(|f| f.kind == "loop-pipeline").collect();
        assert_eq!(pipes.len(), 1, "{fs:?}");
        assert_eq!(pipes[0].function, "collect_other", "{fs:?}");
    }

    #[test]
    fn loop_in_match_arm_expression_is_judged() {
        // a match arm body is an expression and can BE the loop site; the
        // statement-list relation cannot represent that, so the walk handles
        // the expression and keeps the arm index in the path.
        let src = include_str!("../../../tests/fixtures/code/sample_collect__bb6fc063.rs");
        let fs = scan(src);
        assert!(has_kind(&fs, "loop-pipeline"), "{fs:?}");
    }

    #[test]
    fn loop_while_never_pipeline_but_mutation_counts() {
        // a while condition is not an iterator — no combinator replaces it —
        // but its state writes still count for the stateful verdicts
        let fs = scan(include_str!("../../../tests/fixtures/code/sample_f__118503a2.rs"));
        assert!(!has_kind(&fs, "loop-pipeline"), "{fs:?}");
        let multi = scan(include_str!("../../../tests/fixtures/code/sample_f__9c1a0eab.rs"));
        assert!(has_kind(&multi, "mutating-loop"), "{multi:?}");
    }

    #[test]
    fn loop_suppression_with_why_exempts() {
        let src = include_str!("../../../tests/fixtures/code/sample_collect__cd980694.rs");
        assert!(
            !has_kind(&scan(src), "loop-pipeline"),
            "the why'd suppression must exempt"
        );
        let bare = include_str!("../../../tests/fixtures/code/sample_collect__428dc879.rs");
        assert!(has_kind(&scan(bare), "suppression"));
    }
}

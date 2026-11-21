//! The repo-wide families computed from the code-review-graph contract
//! (a versioned, schema-neutral JSON emitted by lucidlint_graph_export.py
//! through the graph tool's own public API — the gate never touches the
//! SQLite schema or the DB location). Plus hotspot (git churn + max CC) and
//! churn-untested (top churn with no test coverage).
//!
//! Finding identity (kind/file/line/function) matches the Python reference
//! exactly; the concern-mix wording in messages is the honest core fact.

mod churn;
mod cohesion;
mod contract;
mod coverage;
mod cycles;
mod folder_mix;
mod graph_paths;
mod hotspot;
mod layer_mix;
mod node;
mod risk;

pub(crate) use churn::ChurnUntested;
pub(crate) use cohesion::Cohesion;
pub use contract::GraphContract;
pub use coverage::CoverageSnapshot;
pub(crate) use cycles::Cycles;
pub(crate) use folder_mix::FolderMix;
pub(crate) use hotspot::Hotspot;
pub(crate) use layer_mix::LayerMix;
pub(crate) use risk::HighRisk;

#[cfg(test)]
mod tests {
    use super::contract::GEdge;
    use super::graph_paths::GraphPaths;
    use super::node::GNode;
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    fn node(kind: &str, name: &str, qn: &str, file: &str, ls: i64, _le: i64, community: Option<i64>) -> GNode {
        GNode {
            kind: kind.into(),
            name: name.into(),
            qualified_name: qn.into(),
            file_path: file.into(),
            line_start: Some(ls),
            community_id: community,
        }
    }

    fn edge(kind: &str, src: &str, tgt: &str, file: &str) -> GEdge {
        GEdge {
            kind: kind.into(),
            source: src.into(),
            target: tgt.into(),
            file_path: file.into(),
        }
    }

    fn contract(nodes: Vec<GNode>, edges: Vec<GEdge>) -> GraphContract {
        GraphContract {
            contract_version: 1,
            nodes,
            edges,
            communities: HashMap::new(),
        }
    }

    fn rels(files: &[&str]) -> Vec<String> {
        files.iter().map(|f| (*f).to_string()).collect()
    }

    #[test]
    fn high_risk_untested_hot_function_is_found() {
        // a function called 12 times, untested, name with a security keyword
        let mut edges = Vec::new();
        for i in 0..12 {
            edges.push(edge(
                "CALLS",
                &format!("/repo/x.py::caller{i}"),
                "/repo/a.py::login",
                "/repo/x.py",
            ));
        }
        let c = contract(
            vec![node("Function", "login", "/repo/a.py::login", "/repo/a.py", 1, 5, None)],
            edges,
        );
        let f = HighRisk::findings(Path::new("/repo"), &c, 0.8, false);
        assert_eq!(f.len(), 1);
        // 0.3 (12 callers) + 0.3 (untested) + 0.4 (security keyword) capped at 1.0
        assert!(f[0].message.contains("high risk score 1.00"));
        assert!(f[0].message.contains("no test coverage"));
    }

    #[test]
    fn high_risk_tested_function_passes() {
        let mut edges = vec![edge("CALLS", "/repo/x.py::c", "/repo/a.py::f", "/repo/x.py")];
        for i in 0..5 {
            edges.push(edge(
                "CALLS",
                &format!("/repo/x.py::c{i}"),
                "/repo/a.py::f",
                "/repo/x.py",
            ));
        }
        edges.push(edge(
            "TESTED_BY",
            "/repo/tests/test_a.py::test_f",
            "/repo/a.py::f",
            "/repo/tests/test_a.py",
        ));
        let c = contract(
            vec![node("Function", "f", "/repo/a.py::f", "/repo/a.py", 1, 5, None)],
            edges,
        );
        // 6 callers -> 0.15, tested -> no 0.3, no keyword -> 0.15 < 0.8
        assert!(HighRisk::findings(Path::new("/repo"), &c, 0.8, false).is_empty());
    }

    #[test]
    fn hotspot_churn_and_cc_gate() {
        let mut churn = HashMap::new();
        churn.insert("a.py".to_string(), 11usize); // top churn -> the candidate
        churn.insert("b.py".to_string(), 10usize);
        let mut cc = HashMap::new();
        cc.insert("a.py".to_string(), 20u32);
        cc.insert("b.py".to_string(), 5u32);
        let f = Hotspot::findings(&churn, &cc, 0.5, 15);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].file, "a.py");
    }

    fn coverage(source: &str, stale: bool, files: &[(&str, &[usize])]) -> CoverageSnapshot {
        CoverageSnapshot {
            source: source.into(),
            stale,
            covered: files
                .iter()
                .map(|(rel, lines)| ((*rel).to_string(), lines.to_vec()))
                .collect(),
        }
    }

    #[test]
    fn churn_untested_snapshot_without_a_covered_line_is_found() {
        let mut churn = HashMap::new();
        churn.insert("a.py".to_string(), 11usize); // top churn -> the candidate
        churn.insert("b.py".to_string(), 10usize);
        // a.py is NAMED by the snapshot with zero covered lines -> unverified
        let cov = coverage("coverage.xml", false, &[("a.py", &[]), ("b.py", &[1, 2])]);
        let f = ChurnUntested::findings(&churn, Some(&cov), 0.5);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].file, "a.py");
        assert_eq!(f[0].kind, "churn-untested");
        assert_eq!(f[0].severity, "fail");
        assert_eq!(
            f[0].message,
            "changed 11x (top 1 by churn) and coverage.xml shows no covered line — volatile and unverified: add a test that exercises it"
        );
    }

    #[test]
    fn churn_untested_covered_file_passes() {
        let mut churn = HashMap::new();
        churn.insert("a.py".to_string(), 11usize);
        let cov = coverage("coverage.xml", false, &[("a.py", &[1, 2, 3])]);
        assert!(ChurnUntested::findings(&churn, Some(&cov), 0.5).is_empty());
    }

    #[test]
    fn churn_untested_file_absent_from_the_snapshot_is_unknown() {
        // R6: absence is not evidence — setup.py and .rs files are simply not
        // in a Python coverage snapshot, and that is UNKNOWN, never uncovered
        let mut churn = HashMap::new();
        churn.insert("setup.py".to_string(), 11usize);
        churn.insert("scanner/src/main.rs".to_string(), 10usize);
        let cov = coverage("coverage.xml", false, &[("lucidlint.py", &[1])]);
        assert!(ChurnUntested::findings(&churn, Some(&cov), 0.5).is_empty());
    }

    #[test]
    fn churn_untested_no_snapshot_makes_no_claim() {
        let mut churn = HashMap::new();
        churn.insert("a.py".to_string(), 11usize);
        assert!(ChurnUntested::findings(&churn, None, 0.5).is_empty());
    }

    #[test]
    fn churn_untested_docs_and_tests_are_not_in_the_population() {
        // docs cannot have tests and test files are the tests — neither takes
        // a slot in the top-N, so a churny README never displaces a code file
        let mut churn = HashMap::new();
        churn.insert("AGENTS.md".to_string(), 40usize);
        churn.insert("tests/test_a.py".to_string(), 30usize);
        churn.insert("a.py".to_string(), 2usize);
        let cov = coverage("coverage.xml", false, &[("a.py", &[])]);
        let f = ChurnUntested::findings(&churn, Some(&cov), 0.5);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].file, "a.py");
        assert_eq!(f[0].message, "changed 2x (top 1 by churn) and coverage.xml shows no covered line — volatile and unverified: add a test that exercises it");
    }

    #[test]
    fn churn_untested_stale_snapshot_reports_verify_as_a_warning() {
        let mut churn = HashMap::new();
        churn.insert("a.py".to_string(), 11usize);
        let cov = coverage("coverage.xml", true, &[("a.py", &[])]);
        let f = ChurnUntested::findings(&churn, Some(&cov), 0.5);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, "warn");
        assert!(
            f[0].message.contains("re-run coverage, then verify"),
            "{}",
            f[0].message
        );
        assert!(
            !f[0].message.contains("add a test"),
            "stale data never demands a test: {}",
            f[0].message
        );
    }

    #[test]
    fn churn_untested_low_churn_passes() {
        // changed once: in the top cutoff but below the volatile gate
        let mut churn = HashMap::new();
        churn.insert("a.py".to_string(), 1usize);
        let cov = coverage("coverage.xml", false, &[("a.py", &[])]);
        assert!(ChurnUntested::findings(&churn, Some(&cov), 0.5).is_empty());
    }

    #[test]
    fn import_cycle_is_found_with_real_files() {
        // module_to_file needs the files to exist — use a temp dir
        let dir = std::env::temp_dir().join(format!("crg_cycle_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("pkg")).unwrap();
        std::fs::write(dir.join("pkg/a.py"), "").unwrap();
        std::fs::write(dir.join("pkg/b.py"), "").unwrap();
        let a_path = format!("{}/pkg/a.py", dir.display());
        let b_path = format!("{}/pkg/b.py", dir.display());
        let c = contract(
            vec![],
            vec![
                edge("IMPORTS_FROM", &a_path, "pkg.b", ""),
                edge("IMPORTS_FROM", &b_path, "pkg.a", ""),
            ],
        );
        let f = Cycles::findings(&dir, &c);
        assert_eq!(f.len(), 1);
        assert!(f[0].message.contains("import cycle"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_mix_max_count_community_wins() {
        // pkg/x.py has 1 node in community 1 (first-seen) and 4 in community
        // 2 — the MAX count must assign it to community 2 (the pre-fix code
        // compared the count to itself, so first-seen always won and the
        // spread would have been m1 (4), m2 (2))
        let mut nodes = vec![
            node("Function", "a", "/repo/pkg/a.py::a", "/repo/pkg/a.py", 1, 2, Some(1)),
            node("Function", "b", "/repo/pkg/b.py::b", "/repo/pkg/b.py", 1, 2, Some(1)),
            node("Function", "c", "/repo/pkg/c.py::c", "/repo/pkg/c.py", 1, 2, Some(1)),
            node("Function", "y", "/repo/pkg/y.py::y", "/repo/pkg/y.py", 1, 2, Some(2)),
            node("Function", "z", "/repo/pkg/z.py::z", "/repo/pkg/z.py", 1, 2, Some(2)),
        ];
        nodes.push(node(
            "Function",
            "x1",
            "/repo/pkg/x.py::x1",
            "/repo/pkg/x.py",
            1,
            2,
            Some(1),
        ));
        for i in 0..4 {
            nodes.push(node(
                "Function",
                &format!("x2_{i}"),
                &format!("/repo/pkg/x.py::x2_{i}"),
                "/repo/pkg/x.py",
                1,
                2,
                Some(2),
            ));
        }
        let mut c = contract(nodes, vec![]);
        c.communities.insert("1".into(), "m1".into());
        c.communities.insert("2".into(), "m2".into());
        let f = FolderMix::findings(Path::new("/repo"), &c);
        let msg = f
            .iter()
            .find(|x| x.kind == "folder-mix")
            .map(|x| x.message.clone())
            .unwrap_or_default();
        // x.py must join the MAX-count community (m2): it must NOT appear in
        // the m1 group that precedes the m2 marker
        let before_m2 = msg.split("m2 (").next().unwrap_or("");
        assert!(
            !before_m2.contains("x.py"),
            "x.py joined first-seen instead of max-count: {msg}"
        );
        assert!(msg.contains("x.py"), "x.py missing from the m2 group: {msg}");
    }

    #[test]
    fn folder_mix_split_across_communities_is_found() {
        let mut c = contract(
            vec![
                node(
                    "Function",
                    "a1",
                    "/repo/pkg/a1.py::a1",
                    "/repo/pkg/a1.py",
                    1,
                    2,
                    Some(1),
                ),
                node(
                    "Function",
                    "a2",
                    "/repo/pkg/a2.py::a2",
                    "/repo/pkg/a2.py",
                    1,
                    2,
                    Some(1),
                ),
                node(
                    "Function",
                    "b1",
                    "/repo/pkg/b1.py::b1",
                    "/repo/pkg/b1.py",
                    1,
                    2,
                    Some(2),
                ),
                node(
                    "Function",
                    "b2",
                    "/repo/pkg/b2.py::b2",
                    "/repo/pkg/b2.py",
                    1,
                    2,
                    Some(2),
                ),
                node(
                    "Function",
                    "b3",
                    "/repo/pkg/b3.py::b3",
                    "/repo/pkg/b3.py",
                    1,
                    2,
                    Some(2),
                ),
            ],
            vec![],
        );
        c.communities.insert("1".into(), "models".into());
        c.communities.insert("2".into(), "views".into());
        let f = FolderMix::findings(Path::new("/repo"), &c);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].file, "pkg");
    }

    #[test]
    fn module_cohesion_two_domains_in_one_file_is_found() {
        // a.py holds 4 nodes in community 1 and 4 in community 2 — two
        // sub-domains in one module, over the edge bar: split candidate.
        let mut nodes = Vec::new();
        for i in 0..4 {
            nodes.push(node(
                "Function",
                &format!("tok{i}"),
                &format!("/repo/layout.py::tok{i}"),
                "/repo/layout.py",
                1,
                2,
                Some(1),
            ));
            nodes.push(node(
                "Function",
                &format!("box{i}"),
                &format!("/repo/layout.py::box{i}"),
                "/repo/layout.py",
                1,
                2,
                Some(2),
            ));
        }
        let mut c = contract(
            nodes,
            vec![
                edge(
                    "CALLS",
                    "/repo/layout.py::tok0",
                    "/repo/layout.py::tok1",
                    "/repo/layout.py",
                ),
                edge(
                    "CALLS",
                    "/repo/layout.py::box0",
                    "/repo/layout.py::box1",
                    "/repo/layout.py",
                ),
            ],
        );
        c.communities.insert("1".into(), "tokenization".into());
        c.communities.insert("2".into(), "box math".into());
        let f = Cohesion::findings(Path::new("/repo"), &c, 2);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].kind, "module-cohesion");
        assert!(f[0].message.contains("tokenization"), "{:?}", f[0].message);
        assert!(f[0].message.contains("box math"), "{:?}", f[0].message);
    }

    #[test]
    fn module_cohesion_single_domain_passes() {
        // one community = one sub-domain — not a split candidate
        let mut nodes = Vec::new();
        for i in 0..8 {
            nodes.push(node(
                "Function",
                &format!("f{i}"),
                &format!("/repo/layout.py::f{i}"),
                "/repo/layout.py",
                1,
                2,
                Some(1),
            ));
        }
        let c = contract(nodes, vec![]);
        assert!(Cohesion::findings(Path::new("/repo"), &c, 2).is_empty());
    }

    #[test]
    fn module_cohesion_below_edge_bar_passes() {
        // two domains but the module is small — the edge bar is the guard
        let mut nodes = Vec::new();
        for i in 0..2 {
            nodes.push(node(
                "Function",
                &format!("a{i}"),
                &format!("/repo/small.py::a{i}"),
                "/repo/small.py",
                1,
                2,
                Some(1),
            ));
            nodes.push(node(
                "Function",
                &format!("b{i}"),
                &format!("/repo/small.py::b{i}"),
                "/repo/small.py",
                1,
                2,
                Some(2),
            ));
        }
        let c = contract(nodes, vec![]);
        assert!(Cohesion::findings(Path::new("/repo"), &c, 8).is_empty());
    }

    #[test]
    fn layer_mix_ignores_path_prefixed_stdlib_calls() {
        // `Vec::new()` is recorded as `<caller-file>::new`; the resolver used
        // to take the first node named `new` — a test file — and invent a
        // second layer for every function that allocated a collection.
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        for i in 0..4 {
            let qn = format!("/repo/houses/mix.py::f{i}");
            nodes.push(node(
                "Function",
                &format!("f{i}"),
                &qn,
                "/repo/houses/mix.py",
                1,
                5,
                None,
            ));
            edges.push(edge("CALLS", &qn, "/repo/houses/mix.py::new", "/repo/houses/mix.py"));
        }
        for i in 0..2 {
            let qn = format!("/repo/houses/mix.py::g{i}");
            nodes.push(node(
                "Function",
                &format!("g{i}"),
                &qn,
                "/repo/houses/mix.py",
                6,
                9,
                None,
            ));
            edges.push(edge(
                "CALLS",
                &qn,
                &format!("/repo/houses/nodes/t{i}.py::t{i}"),
                "/repo/houses/mix.py",
            ));
        }
        // the only node named `new` lives in a test file — the trap
        nodes.push(node(
            "Function",
            "new",
            "/repo/scanner/tests/cli.rs::new",
            "/repo/scanner/tests/cli.rs",
            1,
            2,
            None,
        ));
        let c = contract(nodes, edges);
        assert!(LayerMix::findings(Path::new("/repo"), &c, &rels(&["houses/mix.py"])).is_empty());
    }

    #[test]
    fn layer_mix_ignores_ambiguous_bare_names() {
        // `visit::visit_expr` matches a node in two subsystems — an ambiguous
        // name says nothing about which layer the caller belongs to.
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        for i in 0..4 {
            let qn = format!("/repo/houses/mix.py::f{i}");
            nodes.push(node(
                "Function",
                &format!("f{i}"),
                &qn,
                "/repo/houses/mix.py",
                1,
                5,
                None,
            ));
            edges.push(edge("CALLS", &qn, "visit::visit_expr", "/repo/houses/mix.py"));
        }
        for (file, qn) in [
            (
                "/repo/scanner/radonc/src/visitors.rs",
                "/repo/scanner/radonc/src/visitors.rs::visit_expr",
            ),
            ("/repo/scanner/src/main.rs", "/repo/scanner/src/main.rs::visit_expr"),
        ] {
            nodes.push(node("Function", "visit_expr", qn, file, 1, 2, None));
        }
        let c = contract(nodes, edges);
        assert!(LayerMix::findings(Path::new("/repo"), &c, &rels(&["houses/mix.py"])).is_empty());
    }

    #[test]
    fn layer_mix_dominant_callee_split_is_found() {
        // a file with 6 functions: 3 call into one subsystem, 3 into another
        let mut edges = Vec::new();
        for i in 0..3 {
            edges.push(edge(
                "CALLS",
                &format!("/repo/houses/mix.py::f{i}"),
                &format!("/repo/houses/nodes/target{i}.py::t{i}"),
                "/repo/houses/mix.py",
            ));
        }
        for i in 0..3 {
            edges.push(edge(
                "CALLS",
                &format!("/repo/houses/mix.py::g{i}"),
                &format!("/repo/houses/sheets/target{i}.py::t{i}"),
                "/repo/houses/mix.py",
            ));
        }
        let mut c = contract(
            vec![
                node(
                    "Function",
                    "f0",
                    "/repo/houses/mix.py::f0",
                    "/repo/houses/mix.py",
                    1,
                    2,
                    None,
                ),
                node(
                    "Function",
                    "f1",
                    "/repo/houses/mix.py::f1",
                    "/repo/houses/mix.py",
                    1,
                    2,
                    None,
                ),
                node(
                    "Function",
                    "f2",
                    "/repo/houses/mix.py::f2",
                    "/repo/houses/mix.py",
                    1,
                    2,
                    None,
                ),
                node(
                    "Function",
                    "g0",
                    "/repo/houses/mix.py::g0",
                    "/repo/houses/mix.py",
                    1,
                    2,
                    None,
                ),
                node(
                    "Function",
                    "g1",
                    "/repo/houses/mix.py::g1",
                    "/repo/houses/mix.py",
                    1,
                    2,
                    None,
                ),
                node(
                    "Function",
                    "g2",
                    "/repo/houses/mix.py::g2",
                    "/repo/houses/mix.py",
                    1,
                    2,
                    None,
                ),
                node(
                    "Function",
                    "t0",
                    "/repo/houses/nodes/target0.py::t0",
                    "/repo/houses/nodes/target0.py",
                    1,
                    2,
                    None,
                ),
                node(
                    "Function",
                    "t1",
                    "/repo/houses/sheets/target1.py::t1",
                    "/repo/houses/sheets/target1.py",
                    1,
                    2,
                    None,
                ),
            ],
            edges,
        );
        let _ = &mut c;
        let f = LayerMix::findings(Path::new("/repo"), &c, &["houses/mix.py".to_string()]);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].file, "houses/mix.py");
    }

    #[test]
    fn module_key_and_repo_rel() {
        assert_eq!(
            GraphPaths::module_key(Path::new("/repo"), "/repo/houses/nodes/x.py"),
            "houses/nodes"
        );
        assert_eq!(GraphPaths::repo_rel(Path::new("/repo"), "/repo/a/b.py"), "a/b.py");
        assert!(GraphPaths::is_test_rel("tests/x.py"));
        assert!(!GraphPaths::is_test_rel("houses/x.py"));
    }
}

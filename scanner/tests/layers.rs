//! Architecture-layer self-check: the scan-core modules are standalone —
//! only main (the composition root) may wire them together.
//!
//! Exact-path assertions, never the vacuous `./`-glob form (the
//! archunitpython-glob-rules lesson applies to any glob-based variant).

use std::path::Path;

#[test]
fn scan_core_modules_do_not_depend_on_each_other() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    // (module, forbidden crate:: targets) — each scan-core module must not
    // import another scan-core module; lsp may use main's scan entry point.
    let rules: &[(&str, &[&str])] = &[
        ("checks.rs", &["crate::graph_families", "crate::docs", "crate::lsp"]),
        (
            "checks/shared_path.rs",
            &["crate::graph_families", "crate::docs", "crate::lsp"],
        ),
        (
            "checks/duplicate_expression.rs",
            &["crate::graph_families", "crate::docs", "crate::lsp"],
        ),
        (
            "checks/module_size.rs",
            &["crate::graph_families", "crate::docs", "crate::lsp"],
        ),
        ("graph_families", &["crate::checks", "crate::docs", "crate::lsp"]),
        ("docs.rs", &["crate::checks", "crate::graph_families", "crate::lsp"]),
        ("lsp.rs", &["crate::checks", "crate::graph_families", "crate::docs"]),
    ];
    for (file, forbidden) in rules {
        // A module may be a file or a directory of submodules; either way
        // every source it owns is held to the same rule.
        let mut roots = vec![src_dir.join(file)];
        if !roots[0].is_file() {
            roots = std::fs::read_dir(src_dir.join(file))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
        }
        for root in roots {
            let src = std::fs::read_to_string(&root).unwrap();
            for path in *forbidden {
                assert!(
                    !src.contains(path),
                    "{} must not depend on {path} — the scan-core modules are standalone; only main composes them",
                    root.display()
                );
            }
        }
    }
}

//! `skipif` for Rust: an `#[ignore]`d test — a parked test rots, so the
//! attribute itself is the finding.

use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{File, ItemFn};

use crate::finding::Finding;

use super::ast::has_attr;

pub(crate) fn ignored_test_findings(file: &File, file_name: &str) -> Vec<Finding> {
    struct Collector {
        findings: Vec<Finding>,
        file: String,
    }
    impl<'ast> Visit<'ast> for Collector {
        fn visit_item_fn(&mut self, f: &'ast ItemFn) {
            if has_attr(&f.attrs, "test") && has_attr(&f.attrs, "ignore") {
                self.findings.push(Finding {
                    logical_start: None,
                    seam_members: Vec::new(),
                    file: self.file.clone(),
                    line: f.sig.span().start().line,
                    col: 0,
                    function: f.sig.ident.to_string(),
                    kind: "skipif".into(),
                    severity: "fail".into(),
                    message: format!(
                        "#[ignore]d test '{}' — skipped tests rot; fix the test or delete it, never park it",
                        f.sig.ident
                    ),
                });
            }
            visit::visit_item_fn(self, f);
        }
    }
    let mut c = Collector {
        findings: Vec::new(),
        file: file_name.to_string(),
    };
    c.visit_file(file);
    c.findings
}

// --------------------------------------------------------------------------- comment extraction

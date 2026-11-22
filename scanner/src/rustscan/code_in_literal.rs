//! `code-in-literal` for Rust: a multi-line string holding a code block, or a
//! snippet passed to a scan/compile/exec helper. `include_str!` and the
//! formatting macros are readers and messages, not the finding.

use std::collections::HashSet;

use syn::visit::Visit;
use syn::{Expr, File, Lit, LitStr};

use crate::code_text::{is_code_block, is_code_helper, is_code_snippet};
use crate::finding::Finding;

const MESSAGE: &str =
    "this string is code a reader cannot lint or run; move it to a real file under tests/fixtures/ and read the file.";

pub(crate) fn code_in_literal_findings(file: &File, file_name: &str) -> Vec<Finding> {
    let mut probe = LiteralProbe {
        file_name,
        findings: Vec::new(),
        seen: HashSet::new(),
    };
    probe.visit_file(file);
    probe.findings
}

struct LiteralProbe<'a> {
    file_name: &'a str,
    findings: Vec<Finding>,
    /// (line, column) of a literal already reported — a helper argument is
    /// visited by both the call and the literal hooks.
    seen: HashSet<(usize, usize)>,
}

impl LiteralProbe<'_> {
    fn report_first_arg(&mut self, args: &syn::punctuated::Punctuated<Expr, syn::Token![,]>) {
        if let Some(Expr::Lit(lit)) = args.first() {
            if let Lit::Str(literal) = &lit.lit {
                if is_code_snippet(literal.value().as_str()) {
                    self.report(literal);
                }
            }
        }
    }

    fn report(&mut self, literal: &LitStr) {
        let start = literal.span().start();
        if !self.seen.insert((start.line, start.column)) {
            return;
        }
        self.findings.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            file: self.file_name.to_string(),
            line: start.line,
            col: 0,
            function: String::new(),
            kind: "code-in-literal".into(),
            severity: "warn".into(),
            message: MESSAGE.to_string(),
        });
    }
}

impl<'ast> Visit<'ast> for LiteralProbe<'_> {
    /// A macro's tokens are never a file the reader can lint: `include_str!`
    /// is the target, and `format!`/`println!`/`assert!` carry messages. A
    /// macro's argument semantics are not knowable here, so descending would
    /// flag every message that happens to look like code. Do not descend into
    /// any macro.
    fn visit_macro(&mut self, _mac: &'ast syn::Macro) {}

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        let helper = match &*call.func {
            Expr::Path(path) => path
                .path
                .segments
                .last()
                .is_some_and(|segment| is_code_helper(segment.ident.to_string().as_str())),
            _ => false,
        };
        if helper {
            self.report_first_arg(&call.args);
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        // a method-form scan helper (`self.scan_src`) is the same call; the
        // compile/exec/eval forms are free functions, not methods
        if matches!(call.method.to_string().as_str(), "scan" | "scan_src" | "scan_source") {
            self.report_first_arg(&call.args);
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_lit_str(&mut self, literal: &'ast LitStr) {
        if is_code_block(literal.value().as_str()) {
            self.report(literal);
        }
    }
}

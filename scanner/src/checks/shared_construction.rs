//! `shared-construction`: the same constructor call — the same class, the same
//! builtin conversion of its raw arguments — written at more than one site. The
//! type already has a name; the conversion does not. Add one named constructor
//! to the class and call it everywhere.
//!
//! This is the small-call gap between `duplicate` (whole functions) and
//! `duplicate-expression` (15+ node expressions): `GeoPoint(float(lat),
//! float(lng))` is under the expression floor, and its enclosing functions are
//! not similar, so neither rule sees it.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::visitor::source_order::SourceOrderVisitor;
use ruff_python_ast::{Expr, ExprCall, Stmt};
use ruff_text_size::Ranged;

use crate::{finding::Finding, source::Source, ScanState};

use super::duplicate_expression::ExpressionOccurrence;

/// How many files the message names before counting the rest.
const FILES_SHOWN: usize = 3;

/// The builtin conversions a raw source value is put through. A call whose
/// arguments are already values (`GeoPoint(lat, lon)`) is direct construction,
/// not a shared conversion.
const CONVERSIONS: &[&str] = &["float", "int", "str", "bool"];

/// One construction site: the class, the conversion shape, and where it is.
#[derive(Clone)]
pub struct ConstructionOccurrence {
    pub callee: String,
    pub shape: String,
    pub line: usize,
    pub function: String,
    pub text: String,
}

impl ConstructionOccurrence {
    /// Collect the calls in one module body. The repo-wide pass resolves the
    /// callee against the repo's classes; this walk only records the shape.
    pub(crate) fn collect(state: &mut ScanState, body: &[Stmt], source: &str) {
        struct Probe<'a> {
            source: &'a str,
            function: String,
            occurrences: Vec<ConstructionOccurrence>,
        }
        impl<'a> ruff_python_ast::visitor::source_order::SourceOrderVisitor<'a> for Probe<'a> {
            fn visit_stmt(&mut self, stmt: &'a Stmt) {
                let Stmt::FunctionDef(f) = stmt else {
                    ruff_python_ast::visitor::source_order::walk_stmt(self, stmt);
                    return;
                };
                let was = std::mem::replace(&mut self.function, f.name.to_string());
                ruff_python_ast::visitor::source_order::walk_stmt(self, stmt);
                self.function = was;
            }

            fn visit_expr(&mut self, expr: &'a Expr) {
                if let Expr::Call(call) = expr {
                    if let Some(occ) = ConstructionOccurrence::from_call(call, expr, self.source, &self.function) {
                        self.occurrences.push(occ);
                    }
                }
                ruff_python_ast::visitor::source_order::walk_expr(self, expr);
            }
        }
        let mut probe = Probe {
            source,
            function: String::new(),
            occurrences: Vec::new(),
        };
        for stmt in body {
            probe.visit_stmt(stmt);
        }
        state.shared_constructions.extend(probe.occurrences);
    }

    /// One call that converts at least one raw argument with a builtin, is not
    /// itself a conversion, has no keywords, and stays under the
    /// `duplicate-expression` floor (that rule owns the larger shapes).
    fn from_call(call: &ExprCall, whole: &Expr, source: &str, function: &str) -> Option<Self> {
        let callee = Self::callee_name(call.func.as_ref())?;
        if CONVERSIONS.contains(&callee.as_str()) || !call.arguments.keywords.is_empty() {
            return None;
        }
        if call.arguments.args.is_empty()
            || ExpressionOccurrence::expr_node_count(whole) >= ExpressionOccurrence::EXPRESSION_NODE_FLOOR
        {
            return None;
        }
        let classified: Vec<(String, bool)> = call
            .arguments
            .args
            .iter()
            .map(Self::classify)
            .collect::<Option<Vec<_>>>()?;
        if !classified.iter().any(|(_, converted)| *converted) {
            return None;
        }
        let shape: Vec<String> = classified.into_iter().map(|(kind, _)| kind).collect();
        let args = shape
            .iter()
            .map(|kind| match kind.strip_prefix("conv:") {
                Some(conversion) => format!("{conversion}(...)"),
                None => "...".to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        Some(Self {
            callee: callee.clone(),
            shape: shape.join("|"),
            line: Source::line_of(source, call.range().start()),
            function: function.to_string(),
            text: format!("{callee}({args})"),
        })
    }

    /// The repo-wide merge: one finding per class+shape group that appears at
    /// two or more functions, at the first site. The callee must be a class the
    /// repo defines, so a stdlib or third-party constructor is never named.
    pub(crate) fn findings(scans: &[(String, Vec<ConstructionOccurrence>)], classes: &HashSet<String>) -> Vec<Finding> {
        Self::groups(scans, classes)
            .into_iter()
            .filter_map(|((callee, _shape), mut occurrences)| Self::finding_for_group(&callee, &mut occurrences))
            .collect()
    }

    /// Group every occurrence whose callee is a class the repo defines, by
    /// class and conversion shape.
    fn groups<'a>(
        scans: &'a [(String, Vec<ConstructionOccurrence>)],
        classes: &HashSet<String>,
    ) -> HashMap<(String, String), Vec<(String, &'a ConstructionOccurrence)>> {
        let mut groups: HashMap<(String, String), Vec<(String, &ConstructionOccurrence)>> = HashMap::new();
        for (rel, occurrences) in scans {
            for occurrence in occurrences {
                if classes.contains(&occurrence.callee) {
                    groups
                        .entry((occurrence.callee.clone(), occurrence.shape.clone()))
                        .or_default()
                        .push((rel.clone(), occurrence));
                }
            }
        }
        groups
    }

    /// One finding for a group at two or more functions, at its first site.
    fn finding_for_group(callee: &str, occurrences: &mut [(String, &ConstructionOccurrence)]) -> Option<Finding> {
        occurrences.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.line.cmp(&b.1.line)));
        let places: HashSet<(&str, &str)> = occurrences
            .iter()
            .map(|(rel, occ)| (rel.as_str(), occ.function.as_str()))
            .collect();
        if places.len() < 2 {
            return None;
        }
        let mut files: Vec<&str> = occurrences.iter().map(|(rel, _)| rel.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        let where_text = if files.len() <= FILES_SHOWN {
            files.join(", ")
        } else {
            format!("{}, {}, and {} more", files[0], files[1], files.len() - 2)
        };
        let (rel, first) = &occurrences[0];
        Some(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            file: rel.clone(),
            line: first.line,
            col: 0,
            function: first.function.clone(),
            kind: "shared-construction".into(),
            severity: "warn".into(),
            message: format!(
                "`{}` is built at {} sites across {} {} ({}). Add one `{}.from_values` constructor that converts the raw fields once, and call it at those sites.",
                first.text,
                occurrences.len(),
                files.len(),
                if files.len() == 1 { "file" } else { "files" },
                where_text,
                callee
            ),
        })
    }

    /// The name a call targets, unwrapping a dotted path to its last segment.
    fn callee_name(expr: &Expr) -> Option<String> {
        match expr {
            Expr::Name(name) => Some(name.id.to_string()),
            Expr::Attribute(attr) => Some(attr.attr.to_string()),
            _ => None,
        }
    }

    /// Classify one argument: a builtin conversion (`float(x)`) or a raw value
    /// already written at the site (a name, a dotted path, an index, a literal).
    fn classify(expr: &Expr) -> Option<(String, bool)> {
        match expr {
            Expr::Call(call) => Self::conversion_call(call).map(|name| (format!("conv:{name}"), true)),
            Expr::Name(_)
            | Expr::Attribute(_)
            | Expr::Subscript(_)
            | Expr::NumberLiteral(_)
            | Expr::StringLiteral(_) => Some(("raw".to_string(), false)),
            _ => None,
        }
    }

    /// The builtin conversion a call is, when it takes one positional argument
    /// and no keywords.
    fn conversion_call(call: &ExprCall) -> Option<String> {
        let name = Self::callee_name(call.func.as_ref())?;
        if !CONVERSIONS.contains(&name.as_str()) {
            return None;
        }
        if !call.arguments.keywords.is_empty() || call.arguments.args.len() != 1 {
            return None;
        }
        Some(name)
    }
}

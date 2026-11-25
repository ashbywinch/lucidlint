//! `shared-construction`: the same constructor call — the same class, the same
//! builtin conversion of its raw arguments — written at more than one place. The
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

/// The class and the conversion shape that define one construction pattern.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ConstructionKey {
    callee: String,
    shape: String,
}

/// One construction place: the pattern, and where it is.
#[derive(Clone)]
pub struct ConstructionOccurrence {
    key: ConstructionKey,
    line: usize,
    function: String,
    text: String,
}

/// A construction place with its file, for the repo-wide merge.
struct Site<'a> {
    rel: &'a str,
    occurrence: &'a ConstructionOccurrence,
}

/// One argument of a constructor call: a builtin conversion, or a value
/// already written at the place.
enum Argument {
    Converted(&'static str),
    Raw,
}

impl Argument {
    fn shape(&self) -> String {
        match self {
            Self::Converted(conversion) => format!("conv:{conversion}"),
            Self::Raw => "raw".to_string(),
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Converted(conversion) => format!("{conversion}(...)"),
            Self::Raw => "...".to_string(),
        }
    }
}

impl ConstructionOccurrence {
    /// The builtin conversions a raw source value is put through. A call whose
    /// arguments are already values (`GeoPoint(lat, lon)`) is direct
    /// construction, not a shared conversion.
    const CONVERSIONS: &[&str] = &["float", "int", "str", "bool"];

    /// Collect the calls in one module body. The repo-wide pass resolves the
    /// callee against the repo's classes; this walk only records the pattern.
    pub(crate) fn collect(state: &mut ScanState, body: &[Stmt], source: &str) {
        struct Probe<'a> {
            source: &'a str,
            function: String,
            occurrences: Vec<ConstructionOccurrence>,
        }
        impl<'a> SourceOrderVisitor<'a> for Probe<'a> {
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
        if Self::CONVERSIONS.contains(&callee.as_str()) || !call.arguments.keywords.is_empty() {
            return None;
        }
        if call.arguments.args.is_empty()
            || ExpressionOccurrence::expr_node_count(whole) >= ExpressionOccurrence::EXPRESSION_NODE_FLOOR
        {
            return None;
        }
        let arguments: Vec<Argument> = call
            .arguments
            .args
            .iter()
            .map(Self::classify)
            .collect::<Option<Vec<_>>>()?;
        if !arguments
            .iter()
            .any(|argument| matches!(argument, Argument::Converted(_)))
        {
            return None;
        }
        let shape = arguments.iter().map(Argument::shape).collect::<Vec<_>>().join("|");
        let text = format!(
            "{callee}({})",
            arguments.iter().map(Argument::text).collect::<Vec<_>>().join(", ")
        );
        Some(Self {
            key: ConstructionKey { callee, shape },
            line: Source::line_of(source, call.range().start()),
            function: function.to_string(),
            text,
        })
    }

    /// The repo-wide merge: one finding per pattern built at two or more
    /// functions, at its first place. The callee must be a class the repo
    /// defines, so a stdlib or third-party constructor is never named.
    pub(crate) fn findings(scans: &[(String, Vec<ConstructionOccurrence>)], classes: &HashSet<String>) -> Vec<Finding> {
        Self::groups(scans, classes)
            .into_iter()
            .filter_map(|(key, mut sites)| Self::finding_for_group(&key, &mut sites))
            .collect()
    }

    /// Group every occurrence whose class the repo defines, by class and
    /// conversion shape.
    fn groups<'a>(
        scans: &'a [(String, Vec<ConstructionOccurrence>)],
        classes: &HashSet<String>,
    ) -> HashMap<ConstructionKey, Vec<Site<'a>>> {
        let mut groups: HashMap<ConstructionKey, Vec<Site<'a>>> = HashMap::new();
        for (rel, occurrences) in scans {
            for occurrence in occurrences {
                if classes.contains(&occurrence.key.callee) {
                    groups
                        .entry(occurrence.key.clone())
                        .or_default()
                        .push(Site { rel, occurrence });
                }
            }
        }
        groups
    }

    /// One finding for a pattern built at two or more functions, at its first
    /// place.
    fn finding_for_group(key: &ConstructionKey, sites: &mut [Site]) -> Option<Finding> {
        sites.sort_by(|a, b| a.rel.cmp(b.rel).then(a.occurrence.line.cmp(&b.occurrence.line)));
        let functions: HashSet<(&str, &str)> = sites
            .iter()
            .map(|site| (site.rel, site.occurrence.function.as_str()))
            .collect();
        if functions.len() < 2 {
            return None;
        }
        let mut files: Vec<&str> = sites.iter().map(|site| site.rel).collect();
        files.sort_unstable();
        files.dedup();
        let where_text = if files.len() <= FILES_SHOWN {
            files.join(", ")
        } else {
            format!("{}, {}, and {} more", files[0], files[1], files.len() - 2)
        };
        let first = &sites[0];
        Some(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            file: first.rel.to_string(),
            line: first.occurrence.line,
            col: 0,
            function: first.occurrence.function.clone(),
            kind: "shared-construction".into(),
            severity: "warn".into(),
            message: format!(
                "`{}` is built in {} places across {} {} ({}). Each copy converts the raw fields alone, so a change can miss the others. Add one named constructor on `{}` (for example `from_values`) and call it everywhere.",
                first.occurrence.text,
                sites.len(),
                files.len(),
                if files.len() == 1 { "file" } else { "files" },
                where_text,
                key.callee
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
    /// already written at the place (a name, a dotted path, an index, a
    /// literal).
    fn classify(expr: &Expr) -> Option<Argument> {
        match expr {
            Expr::Call(call) => Self::conversion_call(call).map(Argument::Converted),
            Expr::Name(_)
            | Expr::Attribute(_)
            | Expr::Subscript(_)
            | Expr::NumberLiteral(_)
            | Expr::StringLiteral(_) => Some(Argument::Raw),
            _ => None,
        }
    }

    /// The builtin conversion a call is, when it takes one positional argument
    /// and no keywords.
    fn conversion_call(call: &ExprCall) -> Option<&'static str> {
        let name = Self::callee_name(call.func.as_ref())?;
        let conversion = Self::CONVERSIONS.iter().find(|candidate| **candidate == name)?;
        if !call.arguments.keywords.is_empty() || call.arguments.args.len() != 1 {
            return None;
        }
        Some(conversion)
    }
}

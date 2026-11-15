//! `shared-path` (#60): several modules spell the same path — `work / batch
//! / "ocr-guess"` — so the path is one value with no home. The scan collects
//! each module's path expressions; the repo-wide merge groups them.

use std::collections::HashSet;

use ruff_python_ast::{Expr, ExprCall, Operator, Stmt, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::{line_of, Finding, ScanState};

/// One module-level path expression: the raw material for the shared-path
/// family. `segments` is the path's shape — names, quoted literals, `<Path>`
/// for a constructor, `<f>...` for an f-string — so two modules that build
/// the same path with different variable names still match.
#[derive(Clone)]
pub struct PathExpression {
    pub line: usize,
    pub function: String,
    pub pair: Option<(String, String)>,
    pub segments: Vec<String>,
}

/// Collect a module-level function's outermost path expressions. Nested
/// functions/classes are their own scopes (the visitor reaches them).
pub fn collect_path_expressions(state: &mut ScanState, f: &StmtFunctionDef) {
    use ruff_python_ast::visitor::source_order::SourceOrderVisitor;
    if state.is_test {
        return;
    }
    let pair = leading_pair(f);
    let mut probe = PathProbe {
        exprs: Vec::new(),
        depth: 0,
    };
    for stmt in &f.body {
        probe.visit_stmt(stmt);
    }
    let source = state.source;
    for expr in probe.exprs {
        let segments = path_segments(expr, source);
        if segments.len() >= 2 {
            state.path_expressions.push(PathExpression {
                line: line_of(source, expr.range().start()),
                function: f.name.to_string(),
                pair: pair.clone(),
                segments,
            });
        }
    }
}

struct PathProbe<'a> {
    exprs: Vec<&'a Expr>,
    depth: usize,
}

impl<'a> ruff_python_ast::visitor::source_order::SourceOrderVisitor<'a> for PathProbe<'a> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if self.depth == 0 && is_path_expression(expr) {
            self.exprs.push(expr);
            self.depth += 1;
            ruff_python_ast::visitor::source_order::walk_expr(self, expr);
            self.depth -= 1;
            return;
        }
        ruff_python_ast::visitor::source_order::walk_expr(self, expr);
    }

    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        if matches!(stmt, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            return;
        }
        ruff_python_ast::visitor::source_order::walk_stmt(self, stmt);
    }
}

fn leading_pair(f: &StmtFunctionDef) -> Option<(String, String)> {
    let mut args = f.parameters.posonlyargs.iter().chain(&f.parameters.args);
    let first = args.next()?;
    let second = args.next()?;
    Some((first.parameter.name.to_string(), second.parameter.name.to_string()))
}

fn is_path_expression(expr: &Expr) -> bool {
    match expr {
        Expr::BinOp(b) => matches!(b.op, Operator::Div),
        Expr::Call(c) => path_call(c),
        _ => false,
    }
}

pub(super) fn path_call(call: &ExprCall) -> bool {
    match call.func.as_ref() {
        Expr::Name(n) => matches!(n.id.as_str(), "Path" | "PurePath"),
        Expr::Attribute(a) => matches!(a.attr.as_str(), "joinpath" | "with_suffix"),
        _ => false,
    }
}

pub(super) fn source_text_of(expr: &Expr, source: &str) -> String {
    let range = expr.range();
    source
        .get(range.start().to_usize()..range.end().to_usize())
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn path_segments(expr: &Expr, source: &str) -> Vec<String> {
    match expr {
        Expr::BinOp(b) if matches!(b.op, Operator::Div) => {
            let mut segments = path_segments(&b.left, source);
            segments.extend(path_segments(&b.right, source));
            segments
        }
        Expr::Call(call) if path_call(call) => match call.func.as_ref() {
            Expr::Name(_) => {
                let mut segments = vec!["<Path>".to_string()];
                if let Some(arg) = call.arguments.args.first() {
                    segments.extend(path_segments(arg, source));
                }
                segments
            }
            Expr::Attribute(a) if a.attr.as_str() == "joinpath" => {
                let mut segments = path_segments(&a.value, source);
                for arg in &call.arguments.args {
                    segments.extend(path_segments(arg, source));
                }
                segments
            }
            Expr::Attribute(a) if a.attr.as_str() == "with_suffix" => {
                let mut segments = path_segments(&a.value, source);
                if let Some(arg) = call.arguments.args.first() {
                    segments.extend(path_segments(arg, source));
                }
                segments
            }
            _ => vec!["<call>".to_string()],
        },
        Expr::StringLiteral(s) => literal_segments(s.value.to_str()),
        Expr::FString(f) => vec![fstring_shape(f)],
        Expr::Name(n) => vec![n.id.to_string()],
        Expr::Attribute(_) => vec![source_text_of(expr, source)],
        _ => vec!["<expr>".to_string()],
    }
}

fn literal_segments(value: &str) -> Vec<String> {
    if let Some(suffix) = suffix_value(value) {
        return vec![format!("<suffix>{suffix}")];
    }
    let parts: Vec<&str> = value.split('/').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        return Vec::new();
    }
    parts.into_iter().map(|part| format!("{part:?}")).collect()
}

/// A file suffix (`.layout.json`) is vocabulary whether it is written as a
/// literal or as the tail of an f-string, so both normalise to `<suffix>...`.
fn suffix_value(value: &str) -> Option<&str> {
    (value.starts_with('.') && value.chars().any(|c| c.is_ascii_alphabetic())).then_some(value)
}

pub(super) fn fstring_shape(f: &ruff_python_ast::ExprFString) -> String {
    use ruff_python_ast::InterpolatedStringElement;
    let mut shape = String::new();
    for part in f.value.elements() {
        match part {
            InterpolatedStringElement::Literal(lit) => shape.push_str(&lit.value),
            InterpolatedStringElement::Interpolation(_) => shape.push_str("<f>"),
        }
    }
    match shape.strip_prefix("<f>") {
        Some(rest) if suffix_value(rest).is_some() => format!("<suffix>{rest}"),
        _ => shape,
    }
}

fn is_domain_literal(segment: &str) -> bool {
    let Some(value) = segment
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| segment.strip_prefix("<f>").filter(|rest| !rest.is_empty()))
        .or_else(|| segment.strip_prefix("<suffix>"))
    else {
        return false;
    };
    if value.len() < 3 || !value.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    if value.starts_with('.') && value.len() <= 6 && value[1..].chars().all(|c| c.is_ascii_alphanumeric()) {
        return false; // a bare extension is not a domain noun
    }
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

fn path_runs(segments: &[String]) -> Vec<Vec<String>> {
    let mut runs = Vec::new();
    for start in 0..segments.len() {
        for end in (start + 1)..=segments.len() {
            let run = &segments[start..end];
            if run.iter().any(|s| is_domain_literal(s)) {
                runs.push(run.to_vec());
            }
        }
    }
    runs
}

#[derive(Default)]
struct RunGroup {
    modules: HashSet<String>,
    functions: HashSet<String>,
    uses: usize,
    paired_uses: usize,
    first: Option<(String, usize, String)>,
    pairs: HashSet<(String, String)>,
}

impl RunGroup {
    fn add(&mut self, rel: &str, expr: &PathExpression) {
        self.modules.insert(rel.to_string());
        self.functions.insert(expr.function.clone());
        self.uses += 1;
        if let Some(pair) = &expr.pair {
            self.paired_uses += 1;
            self.pairs.insert(pair.clone());
        }
        self.first
            .get_or_insert_with(|| (rel.to_string(), expr.line, expr.function.clone()));
    }

    /// The pair named in the message only when every use has it and the run
    /// starts with it; a run that starts mid-path, or one use without the
    /// pair, cannot claim the pair as its owner.
    fn pair_for(&self, run: &[String]) -> Option<(String, String)> {
        if self.pairs.len() != 1 || self.paired_uses != self.uses {
            return None;
        }
        let (a, b) = self.pairs.iter().next()?;
        (run.len() >= 2 && run[0].as_str() == a.as_str() && run[1].as_str() == b.as_str())
            .then(|| (a.clone(), b.clone()))
    }
}

/// One finding per shared path run. A run is reported once for all the modules
/// that build it; a shorter run whose modules are already covered by a longer
/// one is dropped (the longer run carries the receiver context).
pub fn shared_path_findings(scans: &[(String, Vec<PathExpression>)]) -> Vec<Finding> {
    let mut groups: std::collections::HashMap<Vec<String>, RunGroup> = std::collections::HashMap::new();
    for (rel, exprs) in scans {
        for expr in exprs {
            for run in path_runs(&expr.segments) {
                let group = groups.entry(run).or_default();
                group.add(rel, expr);
            }
        }
    }
    let mut candidates: Vec<(Vec<String>, RunGroup)> = groups
        .into_iter()
        .filter(|(run, group)| {
            if run.len() == 1 {
                group.uses >= 3 && group.modules.len() >= 2
            } else {
                group.modules.len() >= 2
            }
        })
        .collect();
    candidates.sort_by(|(a_run, a), (b_run, b)| {
        b.modules
            .len()
            .cmp(&a.modules.len())
            .then(b_run.len().cmp(&a_run.len()))
            .then(b.uses.cmp(&a.uses))
    });
    let mut selected: Vec<(Vec<String>, RunGroup)> = Vec::new();
    for (run, group) in candidates {
        if selected.iter().any(|(_, s)| group.modules.is_subset(&s.modules)) {
            continue;
        }
        selected.push((run, group));
    }
    selected
        .into_iter()
        .filter_map(|(run, group)| shared_path_finding(run, group))
        .collect()
}

fn shared_path_finding(run: Vec<String>, group: RunGroup) -> Option<Finding> {
    let (rel, line, function) = group.first.clone()?;
    let path: Vec<String> = run.iter().map(|s| display_segment(s)).collect();
    let path = path.join(" / ");
    let message = match group.pair_for(&run) {
        Some((a, b)) => format!(
            "{} functions in {} modules build `{path}`; they all take `({a}, {b})`. If the pair and this path are one concept, make them one class, and the path components become its constants.",
            group.functions.len(),
            group.modules.len()
        ),
        None if run.len() == 1 && run[0].starts_with("<suffix>") => format!(
            "{} modules spell the suffix `{path}` in a path; it is one value with no home. Give it one home — a constant on the class that owns the directory, or a shared path module.",
            group.modules.len()
        ),
        None if run.len() == 1 => format!(
            "{} modules spell `{path}` in a path; the name is one value with no home. Give it one home — a constant on the class that owns the directory, or a shared path module.",
            group.modules.len()
        ),
        None => format!(
            "{} modules build `{path}`; the path is one value with no home. Give it one home — the class that owns the root, or a shared path module.",
            group.modules.len()
        ),
    };
    Some(Finding {
        logical_start: None,
        seam_members: Vec::new(),
        col: 0,
        file: rel,
        line,
        function,
        kind: "shared-path".into(),
        severity: "warn".into(),
        message,
    })
}

fn display_segment(segment: &str) -> String {
    match segment {
        "<Path>" => "Path()".to_string(),
        _ if segment.starts_with("<suffix>") => format!("\"{}\"", &segment["<suffix>".len()..]),
        _ if segment.starts_with("<f>") => format!("f\"{}\"", segment.replacen("<f>", "{…}", 1)),
        _ => segment.to_string(),
    }
}

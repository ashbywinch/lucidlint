//! `sibling-value-class`: two classes in one package over the same data —
//! their differences are a field, not a class.

use ruff_python_ast::{Expr, Stmt, StmtClassDef};
use ruff_text_size::Ranged;

use crate::finding::Finding;

use super::middle_man::class_base_names;
use super::sibling_class::{SiblingClass, SiblingMethod};
use super::{annotation_base_name, fn_skeleton, push_stmt_children, stmt_exprs};
use crate::source::Source;

/// `sibling-value-class`: the rule object. Its methods read the package's classes; the evidence and the message live together.
pub struct SiblingValueClass;

impl SiblingValueClass {
    /// The `self.<x>` attribute name of an expression, when it is one.
    fn self_attr_name(e: &Expr) -> Option<String> {
        let Expr::Attribute(a) = e else { return None };
        match a.value.as_ref() {
            Expr::Name(n) if n.id.as_str() == "self" => Some(a.attr.to_string()),
            _ => None,
        }
    }

    /// The annotation's base name, including a string forward reference
    /// (`"BatchPaths"`, `"other_module.BatchPaths"`): the sibling pass compares
    /// class names, and a quoted annotation is a name it must still read.
    fn sibling_annotation_name(ann: &Expr) -> Option<String> {
        if let Some(n) = annotation_base_name(ann) {
            return Some(n);
        }
        let Expr::StringLiteral(s) = ann else { return None };
        let text = s.value.to_str();
        let last = text.rsplit('.').next()?.trim();
        if !last.is_empty() && last.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Some(last.to_string());
        }
        None
    }

    /// The declared type of a field's assigned value: a parameter's annotation, a
    /// constructor's class, or nothing.
    fn inferred_field_type(value: &Expr, params: &std::collections::HashMap<String, String>) -> Option<String> {
        match value {
            Expr::Name(n) => params.get(n.id.as_str()).cloned(),
            Expr::Call(c) => Self::sibling_annotation_name(c.func.as_ref()),
            _ => None,
        }
    }

    pub(super) fn collect_field_assignments(
        stmts: &[Stmt],
        params: &std::collections::HashMap<String, String>,
        fields: &mut std::collections::BTreeSet<String>,
        types: &mut std::collections::BTreeSet<String>,
    ) {
        let mut stack: Vec<&Stmt> = stmts.iter().collect();
        while let Some(s) = stack.pop() {
            match s {
                Stmt::Assign(a) => {
                    for t in &a.targets {
                        if let Some(x) = Self::self_attr_name(t) {
                            fields.insert(x);
                            if let Some(ty) = Self::inferred_field_type(a.value.as_ref(), params) {
                                types.insert(ty);
                            }
                        }
                    }
                }
                Stmt::AnnAssign(a) => {
                    if let Some(x) = Self::self_attr_name(a.target.as_ref()) {
                        fields.insert(x);
                        if let Some(t) = annotation_base_name(a.annotation.as_ref()) {
                            types.insert(t.to_string());
                        }
                    }
                }
                Stmt::AugAssign(a) => {
                    if let Some(x) = Self::self_attr_name(a.target.as_ref()) {
                        fields.insert(x);
                    }
                }
                _ => {}
            }
            push_stmt_children(s, &mut stack);
        }
    }

    /// Every `self.<attr>` read in an expression tree, including the containers
    /// `push_expr_tree` does not descend: a f-string (`f"{self.a}/{self.b}"`) and
    /// a list/tuple literal (`[self.a, self.b]`) are the commonest accessor
    /// bodies, and missing them makes the sibling rule blind to the accessors it
    /// is built to compare.
    fn collect_self_attrs(e: &Expr, out: &mut std::collections::BTreeSet<String>) {
        if let Some(x) = Self::self_attr_name(e) {
            out.insert(x);
        }
        match e {
            Expr::Call(c) => {
                Self::collect_self_attrs(c.func.as_ref(), out);
                for a in &c.arguments.args {
                    Self::collect_self_attrs(a, out);
                }
                for k in &c.arguments.keywords {
                    Self::collect_self_attrs(&k.value, out);
                }
            }
            Expr::Attribute(a) => Self::collect_self_attrs(a.value.as_ref(), out),
            Expr::BinOp(b) => {
                Self::collect_self_attrs(b.left.as_ref(), out);
                Self::collect_self_attrs(b.right.as_ref(), out);
            }
            Expr::BoolOp(b) => b.values.iter().for_each(|v| Self::collect_self_attrs(v, out)),
            Expr::UnaryOp(u) => Self::collect_self_attrs(u.operand.as_ref(), out),
            Expr::Compare(c) => {
                Self::collect_self_attrs(c.left.as_ref(), out);
                c.comparators.iter().for_each(|v| Self::collect_self_attrs(v, out));
            }
            Expr::Subscript(s) => {
                Self::collect_self_attrs(s.value.as_ref(), out);
                Self::collect_self_attrs(s.slice.as_ref(), out);
            }
            Expr::List(l) => l.elts.iter().for_each(|v| Self::collect_self_attrs(v, out)),
            Expr::Tuple(t) => t.elts.iter().for_each(|v| Self::collect_self_attrs(v, out)),
            Expr::Set(st) => st.elts.iter().for_each(|v| Self::collect_self_attrs(v, out)),
            Expr::Dict(d) => {
                for k in d.items.iter().filter_map(|i| i.key.as_ref()) {
                    Self::collect_self_attrs(k, out);
                }
                d.items.iter().for_each(|i| Self::collect_self_attrs(&i.value, out));
            }
            Expr::FString(s) => {
                for part in s.value.elements() {
                    if let ruff_python_ast::InterpolatedStringElement::Interpolation(i) = part {
                        Self::collect_self_attrs(i.expression.as_ref(), out);
                    }
                }
            }
            Expr::If(i) => {
                Self::collect_self_attrs(i.test.as_ref(), out);
                Self::collect_self_attrs(i.body.as_ref(), out);
                Self::collect_self_attrs(i.orelse.as_ref(), out);
            }
            Expr::Starred(st) => Self::collect_self_attrs(st.value.as_ref(), out),
            Expr::Await(a) => Self::collect_self_attrs(a.value.as_ref(), out),
            Expr::Named(n) => Self::collect_self_attrs(n.value.as_ref(), out),
            Expr::ListComp(c) => {
                Self::collect_self_attrs(c.elt.as_ref(), out);
                for g in &c.generators {
                    Self::collect_self_attrs(&g.iter, out);
                    g.ifs.iter().for_each(|v| Self::collect_self_attrs(v, out));
                }
            }
            Expr::Generator(c) => {
                Self::collect_self_attrs(c.elt.as_ref(), out);
                for g in &c.generators {
                    Self::collect_self_attrs(&g.iter, out);
                    g.ifs.iter().for_each(|v| Self::collect_self_attrs(v, out));
                }
            }
            Expr::SetComp(c) => {
                Self::collect_self_attrs(c.elt.as_ref(), out);
                for g in &c.generators {
                    Self::collect_self_attrs(&g.iter, out);
                    g.ifs.iter().for_each(|v| Self::collect_self_attrs(v, out));
                }
            }
            _ => {}
        }
    }

    fn self_attributes_read(stmts: &[Stmt]) -> Vec<String> {
        let mut out: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut stack: Vec<&Stmt> = stmts.iter().collect();
        while let Some(s) = stack.pop() {
            for e in stmt_exprs(s) {
                Self::collect_self_attrs(e, &mut out);
            }
            push_stmt_children(s, &mut stack);
        }
        out.into_iter().collect()
    }

    fn sibling_class(c: &StmtClassDef, source: &str) -> SiblingClass {
        let mut fields: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut types: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for m in &c.body {
            if let Stmt::AnnAssign(a) = m {
                if let Expr::Name(n) = a.target.as_ref() {
                    fields.insert(n.id.to_string());
                    if let Some(t) = Self::sibling_annotation_name(a.annotation.as_ref()) {
                        types.insert(t);
                    }
                }
            }
        }
        let mut methods = Vec::new();
        for m in &c.body {
            let Stmt::FunctionDef(f) = m else { continue };
            let mut params: std::collections::HashMap<String, String> = std::collections::HashMap::new();
            let mut mentions: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for p in &f.parameters.args {
                if let Some(t) = p
                    .parameter
                    .annotation
                    .as_ref()
                    .and_then(|a| Self::sibling_annotation_name(a))
                {
                    params.insert(p.parameter.name.to_string(), t.clone());
                    mentions.insert(t);
                }
            }
            if let Some(r) = &f.returns {
                if let Some(t) = Self::sibling_annotation_name(r.as_ref()) {
                    mentions.insert(t);
                }
            }
            Self::collect_field_assignments(&f.body, &params, &mut fields, &mut types);
            methods.push(SiblingMethod {
                name: f.name.to_string(),
                skeleton: fn_skeleton(f),
                uses: Self::self_attributes_read(&f.body),
                mentions: mentions.into_iter().collect(),
            });
        }
        SiblingClass {
            name: c.name.to_string(),
            line: Source::line_of(source, c.name.range().start()),
            fields: fields.into_iter().collect(),
            field_types: types.into_iter().collect(),
            bases: class_base_names(c),
            methods,
        }
    }

    /// The classes a file declares, for the repo-wide sibling pass.
    pub fn collect_sibling_classes(body: &[Stmt], source: &str) -> Vec<SiblingClass> {
        body.iter()
            .filter_map(|s| match s {
                Stmt::ClassDef(c) => Some(Self::sibling_class(c, source)),
                _ => None,
            })
            .collect()
    }

    /// Two classes over the same data in one package: same fields, same derived
    /// accessors, no containment — one of them is the duplicate (issue #61).
    pub fn sibling_value_class_findings(scans: &[(String, Vec<SiblingClass>)]) -> Vec<Finding> {
        use std::collections::BTreeMap;
        let mut per_dir: BTreeMap<String, Vec<(&String, &SiblingClass)>> = BTreeMap::new();
        for (rel, classes) in scans {
            let dir = rel.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string());
            per_dir.entry(dir).or_default().extend(classes.iter().map(|c| (rel, c)));
        }
        let mut out = Vec::new();
        for (_, classes) in per_dir {
            for i in 0..classes.len() {
                for j in (i + 1)..classes.len() {
                    let (_, a) = classes[i];
                    let (rel_b, b) = classes[j];
                    if let Some(f) = a.duplicate_of(rel_b, b) {
                        out.push(f);
                    }
                }
            }
        }
        out
    }
}

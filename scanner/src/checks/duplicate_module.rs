//! `duplicate-module`: two modules or classes with the same structural skeleton —
//! a fork that one edit can leave behind.

use std::collections::HashMap;

use rayon::prelude::*;
use ruff_python_ast::{AnyNodeRef, Expr, Stmt, StmtClassDef};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::paths::Paths;
use crate::skeleton::{dice_from_bigrams, skel_children, BigramInterner, SkeletonModule, Q};
use crate::source::Source;
use crate::ScanState;

use super::class_facts::class_methods;
use super::sibling::SiblingValueClass;

/// The normalized constant token: `NAME:norm(type)` — numeric literals
/// normalised, strings stripped, bools canonical (R4). None for non-literal
/// values (calls, names, collections).
/// The duplicate-module rule: the skeleton of one module and the partner that
/// matches it.
pub struct DuplicateModule;

impl DuplicateModule {
    fn constant_token(target: &str, value: &Expr) -> Option<String> {
        let ty = match value {
            Expr::NumberLiteral(_) => "num",
            Expr::StringLiteral(_) | Expr::BytesLiteral(_) => "str",
            Expr::BooleanLiteral(_) => "bool",
            Expr::NoneLiteral(_) => "none",
            Expr::UnaryOp(u) => return Self::constant_token(target, u.operand.as_ref()),
            _ => return None,
        };
        Some(format!("{target}:{ty}"))
    }

    /// The `NAME = literal` constant tokens in a statement list, sorted+deduped.
    fn constant_tokens(body: &[Stmt]) -> Vec<String> {
        let mut out = Vec::new();
        for s in body {
            let (targets, value): (Vec<&Expr>, Option<&Expr>) = match s {
                Stmt::Assign(a) => (a.targets.iter().collect(), Some(a.value.as_ref())),
                Stmt::AnnAssign(a) => (vec![a.target.as_ref()], a.value.as_deref()),
                _ => (Vec::new(), None),
            };
            let Some(v) = value else { continue };
            for t in targets {
                if let Expr::Name(n) = t {
                    if n.id.as_str().starts_with("__") {
                        continue;
                    }
                    if let Some(tok) = Self::constant_token(n.id.as_str(), v) {
                        out.push(tok);
                    }
                }
            }
        }
        let mut uniq: Vec<String> = out.into_iter().collect();
        uniq.sort();
        uniq.dedup();
        uniq
    }

    /// The fields a module's own classes declare: class-body annotations
    /// (`name: str`) plus the `self.<name> = ...` assignments in their methods —
    /// the field set the sibling pass compares. A record's fields ARE its facts,
    /// so two records whose fields differ are siblings by shape, not copies
    /// (#73: two one-field records were reported as a fork because a field NAME
    /// is an "N" token in the skeleton).
    fn declared_record_fields(classes: &[&StmtClassDef]) -> Vec<String> {
        let mut fields: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for c in classes {
            for m in &c.body {
                if let Stmt::AnnAssign(a) = m {
                    if let Expr::Name(n) = a.target.as_ref() {
                        fields.insert(n.id.to_string());
                    }
                }
                if let Stmt::FunctionDef(f) = m {
                    let mut assigned: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
                    let mut types: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
                    SiblingValueClass::collect_field_assignments(
                        &f.body,
                        &std::collections::HashMap::new(),
                        &mut assigned,
                        &mut types,
                    );
                    fields.extend(assigned);
                }
            }
        }
        fields.into_iter().collect()
    }

    /// One statement's structural tokens, headed by a member-boundary sentinel
    /// so bigrams never cross statement boundaries.
    fn stmt_skeleton<'a>(stmt: &'a Stmt, sentinel: &str, toks: &mut Vec<String>, queue: &mut Vec<Q<'a>>) {
        toks.push(sentinel.to_string());
        queue.push(Q::N(AnyNodeRef::from(stmt)));
        let mut i = 0usize;
        while i < queue.len() {
            let node = match &queue[i] {
                Q::N(n) => *n,
                Q::T(t) => {
                    toks.push((*t).to_string());
                    i += 1;
                    continue;
                }
            };
            i += 1;
            match node {
                AnyNodeRef::ExprName(_) => toks.push("N".to_string()),
                AnyNodeRef::ExprStringLiteral(_)
                | AnyNodeRef::ExprBytesLiteral(_)
                | AnyNodeRef::ExprNumberLiteral(_)
                | AnyNodeRef::ExprBooleanLiteral(_)
                | AnyNodeRef::ExprNoneLiteral(_)
                | AnyNodeRef::ExprEllipsisLiteral(_)
                | AnyNodeRef::InterpolatedStringLiteralElement(_) => toks.push("C".to_string()),
                AnyNodeRef::Parameter(_) | AnyNodeRef::ParameterWithDefault(_) => toks.push("A".to_string()),
                AnyNodeRef::Parameters(_) => toks.push("arguments".to_string()),
                AnyNodeRef::ElifElseClause(_) => toks.push("If".to_string()),
                AnyNodeRef::InterpolatedElement(_) => toks.push("FormattedValue".to_string()),
                _ => toks.push(format!("{:?}", node.kind())),
            }
            skel_children(node, queue);
        }
    }

    /// The structural tokens of a statement list with member boundaries (sentinels).
    fn body_skeleton(body: &[Stmt], toks: &mut Vec<String>) {
        for s in body {
            if matches!(s, Stmt::Import(_) | Stmt::ImportFrom(_)) {
                continue;
            }
            // FRESH queue per statement: a shared queue would restart each statement's
            // walk at index 0 and re-walk every earlier statement's subtree — O(n^2)
            // on big modules, OOM on a 2k-line file
            let mut queue: Vec<Q> = Vec::new();
            Self::stmt_skeleton(s, ";", toks, &mut queue);
        }
    }

    /// Collect a file's duplicate-module identities (#26): the module itself,
    /// each top-level class, and the combined module-class identity when a
    /// module's non-import top-levels are exactly one class def.
    /// A class identity's direct base names ("Name:Base" / "Attr:pkg:Base"),
    /// sorted+deduped — the C2 shared-base pairing signal.
    fn class_bases(c: &StmtClassDef) -> Vec<String> {
        let mut bases: Vec<String> = Vec::new();
        if let Some(arguments) = &c.arguments {
            for b in &arguments.args {
                match b {
                    Expr::Name(n) => bases.push(format!("Name:{}", n.id.as_str())),
                    Expr::Attribute(a) => {
                        if let Expr::Name(v) = a.value.as_ref() {
                            bases.push(format!("Attr:{}:{}", v.id.as_str(), a.attr.as_str()));
                        }
                    }
                    _ => {}
                }
            }
        }
        bases.sort();
        bases.dedup();
        bases
    }

    pub fn collect(state: &mut ScanState, body: &[Stmt], rel: &str) {
        if Self::is_skipped_module(state, rel) {
            return;
        }
        if body.len() < 3 {
            return; // the file >=3-statement floor
        }
        let classes: Vec<&StmtClassDef> = body
            .iter()
            .filter_map(|s| match s {
                Stmt::ClassDef(c) => Some(c),
                _ => None,
            })
            .collect();
        let module_consts = Self::constant_tokens(body);
        let mut module_skel = Vec::new();
        Self::body_skeleton(body, &mut module_skel);
        let mut module_members: Vec<String> = body
            .iter()
            .filter_map(|s| match s {
                Stmt::FunctionDef(f) => Some(f.name.to_string()),
                Stmt::ClassDef(c) => Some(c.name.to_string()),
                _ => None,
            })
            .collect();
        module_members.sort();
        let non_import = body
            .iter()
            .filter(|s| !matches!(s, Stmt::Import(_) | Stmt::ImportFrom(_)))
            .count();
        let module_qualifies = non_import >= 2 && module_skel.len() >= 12;
        if module_qualifies {
            state.skeleton_modules.push(SkeletonModule {
                rel: rel.to_string(),
                name: rel
                    .rsplit('/')
                    .next()
                    .unwrap_or(rel)
                    .trim_end_matches(".py")
                    .to_string(),
                line: 1,
                skeleton: module_skel.clone(),
                consts: module_consts.clone(),
                members: module_members.clone(),
                fields: Self::declared_record_fields(&classes),
                entity: "module",
                bases: Vec::new(),
            });
        }
        for c in &classes {
            let mut skel = Vec::new();
            Self::body_skeleton(&c.body, &mut skel);
            let consts = Self::constant_tokens(&c.body);
            let mut members: Vec<String> = class_methods(c).iter().map(|f| f.name.to_string()).collect();
            members.sort();
            if skel.len() < 12 {
                continue;
            }
            let line = Source::line_of(state.source, c.name.range().start());
            state.skeleton_modules.push(SkeletonModule {
                rel: rel.to_string(),
                name: c.name.to_string(),
                line,
                skeleton: skel.clone(),
                consts: consts.clone(),
                members: members.clone(),
                fields: Self::declared_record_fields(&[c]),
                entity: "class",
                bases: Self::class_bases(c),
            });
            // module-class identity: exactly one class; everything else at the
            // top level is imports or constants
            let other_statements = body
                .iter()
                .filter(|s| {
                    !matches!(s, Stmt::Import(_) | Stmt::ImportFrom(_) | Stmt::ClassDef(_))
                        && !matches!(s, Stmt::Assign(_) | Stmt::AnnAssign(_))
                })
                .count()
                == 0;
            if Self::is_module_class(classes.len(), other_statements, module_qualifies) {
                let mut combined = skel.clone();
                combined.extend(module_consts.iter().cloned());
                let mut all = module_members.clone();
                all.extend(members.iter().cloned());
                all.sort();
                all.dedup();
                state.skeleton_modules.push(SkeletonModule {
                    rel: rel.to_string(),
                    name: c.name.to_string(),
                    line,
                    skeleton: combined,
                    consts: module_consts.clone(),
                    members: all,
                    fields: Self::declared_record_fields(&[c]),
                    entity: "module-class",
                    bases: Vec::new(),
                });
            }
        }
    }

    /// Cross-file module/class forks (#26): two identities with the same
    /// structural skeleton (>= 0.9 Dice) AND identical constants — a fix lands
    /// in one copy while the other silently keeps the old behavior. Pairing
    /// identities: module-module, class-class, and module-class (a module whose
    /// non-import top-levels are exactly one class def). The message names the
    /// INTERSECTING members (matched members:) — the bigram Dice cannot produce
    /// the list, so the member sets are tracked alongside.
    /// A pair is comparable when both sides are the same kind of entity, or when
    /// either side is a whole-file class — a file that is one class pairs with the
    /// class it duplicates.
    fn entity_compatible(a: &SkeletonModule, b: &SkeletonModule) -> bool {
        a.entity == b.entity || a.entity == "module-class" || b.entity == "module-class"
    }

    /// The first later entity whose shape pairs with entity `i`: length within a
    /// fifth, comparable entity kind, the same declared facts, and Dice at or above
    /// the bar. Length buckets narrow the search; the measure decides.
    fn partner_for(
        i: usize,
        entities: &[SkeletonModule],
        interner: &BigramInterner,
        buckets: &HashMap<usize, Vec<usize>>,
        len_keys: &[usize],
    ) -> Option<usize> {
        let e = &entities[i];
        let l = e.skeleton.len();
        let tol = (l / 5).max(2);
        let lo = l.saturating_sub(tol);
        let hi = l + tol;
        for &len in len_keys.iter().filter(|&&k| k >= lo && k <= hi) {
            let bucket = &buckets[&len];
            let start = bucket.partition_point(|&j| j <= i);
            for &j in &bucket[start..] {
                let later = &entities[j];
                if !Self::entity_compatible(e, later) {
                    continue;
                }
                // #73: a record's declared fields are its facts. Two records whose
                // fields differ hold different data — there is no shared behaviour
                // for a fix to land in one copy and miss in the other, so they are
                // siblings, not a fork.
                if Self::fields_differ(e, later) {
                    continue;
                }
                // two classes pair on matching constants as well as on shape: small
                // classes are alike by nature, so the config evidence carries the
                // fork claim there. Modules pair on the structural measure alone — a
                // whole file's shape is evidence enough, and constant-free module
                // forks exist.
                if Self::consts_differ(e, later) {
                    continue;
                }
                if dice_from_bigrams(interner.bigrams_of(i), interner.bigrams_of(j)) >= 0.9 {
                    return Some(j);
                }
            }
        }
        None
    }

    pub fn findings(entities: &[SkeletonModule]) -> Vec<Finding> {
        let mut out = Vec::new();
        if entities.len() < 2 {
            return out;
        }
        let interner = BigramInterner::new(&entities.iter().map(|e| e.skeleton.as_slice()).collect::<Vec<_>>());
        let mut buckets: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, e) in entities.iter().enumerate() {
            buckets.entry(e.skeleton.len()).or_default().push(i);
        }
        let mut len_keys: Vec<usize> = buckets.keys().copied().collect();
        len_keys.sort_unstable();
        let results: Vec<Option<(Finding, String, String, bool)>> = (0..entities.len())
        .into_par_iter()
        .map(|i| {
            let e = &entities[i];
            let j = Self::partner_for(i, entities, &interner, &buckets, &len_keys)?;
            // the finding lands on the LATER entity (list order), naming the
            // earlier one — mirrors duplicate_findings' _first_duplicate
            // semantics, so the earlier copy's readers see the fork at the
            // later copy
            let (earlier, later) = (e, &entities[j]);
            let eset: std::collections::HashSet<&str> = earlier.members.iter().map(|m| m.as_str()).collect();
            let mut matched: Vec<String> = later
                .members
                .iter()
                .filter(|m| eset.contains(m.as_str()))
                .cloned()
                .collect();
            matched.sort();
            let sim = dice_from_bigrams(interner.bigrams_of(i), interner.bigrams_of(j));
            let what = if later.entity == "class" { "classes" } else { "modules" };
            let members_text = if matched.is_empty() {
                String::new()
            } else {
                format!(" — matched members: {}", matched.join(", "))
            };
            // C2: two classes inheriting the SAME base have a shared owner —
            // the fork prose is false there (a fix in one subclass does not
            // silently diverge; the base is the shared home)
            let shared_base = earlier.entity == "class"
                && later.entity == "class"
                && !earlier.bases.is_empty()
                && earlier.bases == later.bases;
            let message = if shared_base {
                format!(
                    "{what}: {a} and {b} are two subclasses of one base with identical shape; only their constants differ. If they are config-variants, move the constants into the base and delete the duplicate subclasses. If they were meant to differ, one is missing its override.",
                    what = what,
                    a = earlier.name,
                    b = later.name,
                )
            } else {
                let consts_clause = if !earlier.consts.is_empty() && earlier.consts == later.consts {
                    format!(" with identical constants {{{}}}", later.consts.join(", "))
                } else {
                    String::new()
                };
                format!(
                    "{what} '{}' ({}:{}) and '{}' ({}:{}) are {:.0}% similar{}{} — a structural match. A fix can land in one copy and silently miss the other. If they are copies, reconcile them into one. If one is generated from the other and a check that fails when they diverge pins them, this is not a fork and the finding does not apply.",
                    earlier.name,
                    earlier.rel,
                    earlier.line,
                    later.name,
                    later.rel,
                    later.line,
                    sim * 100.0,
                    consts_clause,
                    members_text,
                )
            };
            Some((
                Finding { logical_start: None, seam_members: Vec::new(),
                col: 0,
                file: later.rel.clone(),
                line: later.line,
                function: later.name.clone(),
                kind: "duplicate-module".into(),
                severity: "fail".into(),
                message, },
                earlier.rel.clone(),
                later.rel.clone(),
                later.entity == "class",
            ))
        })
        .collect();
        // one finding per file pair, at the most specific level: when a class
        // pair and its enclosing module pair report the same two files, the
        // class finding wins — it names the classes and their relationship.
        let class_pairs: std::collections::HashSet<(String, String)> = results
            .iter()
            .flatten()
            .filter(|(_, _, _, is_class)| *is_class)
            .map(|(_, a, b, _)| (a.clone(), b.clone()))
            .collect();
        for (f, a, b, is_class) in results.into_iter().flatten() {
            if !is_class && class_pairs.contains(&(a, b)) {
                continue;
            }
            out.push(f);
        }
        out
    }

    /// The module is outside the identity scan: test code, a package marker, or
    /// vendored.
    fn is_skipped_module(state: &ScanState, rel: &str) -> bool {
        state.is_test || rel.ends_with("__init__.py") || Paths::is_vendored(rel)
    }

    /// Two class skeletons carry different field sets — siblings, not a fork.
    fn fields_differ(e: &crate::skeleton::SkeletonModule, earlier: &crate::skeleton::SkeletonModule) -> bool {
        !e.fields.is_empty() && !earlier.fields.is_empty() && e.fields != earlier.fields
    }

    /// Two class skeletons pair on constants only when both carry them.
    fn consts_differ(e: &crate::skeleton::SkeletonModule, earlier: &crate::skeleton::SkeletonModule) -> bool {
        e.entity == "class" && earlier.entity == "class" && (e.consts.is_empty() || e.consts != earlier.consts)
    }

    /// The file's shape is one class plus module-level imports and constants.
    fn is_module_class(classes: usize, other_statements: bool, module_qualifies: bool) -> bool {
        classes == 1 && other_statements && module_qualifies
    }
}

//! One file's scanned facts: the per-file output the pipeline merges, and the
//! class/import/self-assignment shapes the repo-wide passes read.

use std::collections::HashSet;

use crate::checks;
use crate::finding::Finding;
use crate::finding::FnCc;
use crate::skeleton::{SkeletonFn, SkeletonModule};

#[derive(Clone)]
pub struct ClassInfo {
    pub name: String,
    pub line: usize,
    pub abstract_: bool,
    pub bases: Vec<String>,
    /// Declared members (class-body annotations, __slots__, annotated
    /// __init__ self-assigns) — undeclared-attribute's declaration table.
    pub declared: Vec<String>,
}

/// One import alias: local name -> (module, imported name).
#[derive(Clone)]
pub struct ImportInfo {
    pub alias: String,
    pub module: String,
    pub imported: String,
}

/// One `self.<attr> = ...` site inside a top-level class — undeclared-
/// attribute's raw material: the repo-wide merge decides which are quiet
/// assignments (per-file scans cannot resolve base classes).
#[derive(Clone)]
pub struct SelfAssign {
    pub class: String,
    pub attr: String,
    pub line: usize,
    pub function: String,
    pub is_init: bool,
}

/// One file's undeclared-attribute inputs: its rel, classes (declared
/// members + bases), imports, and self-assignment sites — the per-scan
/// slice the repo-wide pass resolves.
#[derive(Clone, Default)]
pub struct UndeclScan {
    pub rel: String,
    pub classes: Vec<ClassInfo>,
    pub imports: Vec<ImportInfo>,
    pub self_assigns: Vec<SelfAssign>,
}

/// One definition site in the repo — the `unused` rule's subject: where it is,
/// what it is called, and whether it is a class (the scattered-instance rule
/// resolves against class names).
#[derive(Clone, Debug)]
pub struct Definition {
    pub rel: String,
    pub name: String,
    pub line: usize,
    pub is_class: bool,
}

/// One setter the repo defines — the `unused` rule's second subject: a method
/// that only assigns members.
#[derive(Clone, Debug)]
pub struct Setter {
    pub rel: String,
    pub name: String,
    pub line: usize,
}

/// The repo-wide merge's two suppression maps, keyed by the file's rel: what
/// each file spent, and what remains.
pub type SuppressionsMap = std::collections::HashMap<String, crate::suppressions::Suppressions>;
pub type Spent = std::collections::HashSet<(usize, String)>;
pub type SpentMap = std::collections::HashMap<String, Spent>;

#[derive(Clone, Default)]
pub struct FileScan {
    pub file_name: String,
    pub findings: Vec<Finding>,
    pub cc: Vec<FnCc>,
    pub errors: usize,
    pub defs: Vec<(String, usize)>,
    pub class_defs: Vec<(String, usize)>,
    pub scattered: Vec<checks::ScatteredInstance>,
    /// The file's substantive expressions (#104 duplicate-expression).
    pub duplicate_expressions: Vec<checks::ExpressionOccurrence>,
    /// The file's module-level path expressions (#60 shared-path).
    pub path_expressions: Vec<checks::PathExpression>,
    /// The file's size and concept groups (#107 large-module).
    pub module_shape: checks::ModuleShape,
    pub setters: Vec<(String, usize)>,
    pub refs: HashSet<String>,
    pub strings: Vec<String>,
    pub skeletons: Vec<SkeletonFn>,
    pub skeleton_modules: Vec<SkeletonModule>,
    pub decorated: HashSet<String>,
    pub classes: Vec<ClassInfo>,
    pub imports: Vec<ImportInfo>,
    pub self_assigns: Vec<SelfAssign>,
    /// The file's parsed `lucidlint: ignore` suppressions — the repo-wide
    /// families (duplicate/unused/docs/graph) are filtered through them at
    /// the end of the scan (per-file families apply theirs during the scan).
    pub supps: crate::suppressions::Suppressions,
    /// (line, signal) pairs the per-file pass consumed — stage-2 repo-wide
    /// re-honoring must not spend them again (innermost-peel capacity).
    pub supps_spent: HashSet<(usize, String)>,
    pub forwarder_fns: Vec<checks::ForwarderFn>,
    pub forwarder_methods: Vec<checks::ForwarderMethod>,
    pub repo_methods: Vec<checks::RepoMethod>,
    pub middle_man_classes: Vec<checks::MiddleManClass>,
    pub middle_man_candidates: Vec<checks::MiddleManCandidate>,
    /// The file's class shapes for the repo-wide sibling pass (#61).
    pub sibling_classes: Vec<checks::SiblingClass>,
}

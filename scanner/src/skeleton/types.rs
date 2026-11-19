//! The skeleton records: one function candidate, one module identity, and the
//! BFS queue item.

use ruff_python_ast::AnyNodeRef;

/// One duplicate candidate: the file/name/line plus its structural skeleton.
/// The skeleton is a language-layer product (ruff op tokens for Python, syn
/// tokens for Rust); the similarity machinery below is language-neutral.
#[derive(Clone)]
pub struct SkeletonFn {
    pub rel: String,
    pub name: String,
    pub line: usize,
    /// last line of the definition — a function nested in another always sits
    /// inside its parent's span, which is containment, not copy-paste
    pub end_line: usize,
    pub skeleton: Vec<String>,
}

/// One duplicate-module candidate (#26): a module or top-level class
/// identity with its structural skeleton, constant tokens, and member-name
/// set. Built per file in the Python layer; the repo-wide pass pairs
/// identities with >= 0.9 Dice AND identical constant sets.
#[derive(Clone)]
pub struct SkeletonModule {
    pub rel: String,
    pub name: String,
    pub line: usize,
    pub skeleton: Vec<String>,
    /// `NAME:norm(type)` tokens for the identity's `NAME = literal` constants
    /// (numeric literals normalised, strings stripped, bools canonical).
    pub consts: Vec<String>,
    /// Sorted member names — the issue's "matched members:" intersection.
    pub members: Vec<String>,
    /// Sorted declared field names — class-body annotations plus the
    /// `self.<name> = ...` assignments in methods. A record's fields are its
    /// facts: two identities whose declared fields differ are siblings by
    /// shape, not copies (#73).
    pub fields: Vec<String>,
    /// "module" | "class" | "module-class" (a module whose non-import
    /// top-levels are exactly one class def — pairs as both).
    pub entity: &'static str,
    /// The class identity's direct base names ("Name:Base" / "Attr:mod:Base")
    /// — empty for module/module-class identities. C2: two classes sharing
    /// one base get the subclass message, not the fork prose.
    pub bases: Vec<String>,
}

/// enumeration replicates `ast.iter_child_nodes` field order — the
/// Parameters node emits its defaults hoisted (kw_defaults then defaults),
/// dict literals emit all keys before all values, and if/elif chains
/// re-expand into nested "If" nodes, exactly like CPython's orelse nesting.
/// A BFS queue item — either an AST node or a bare operator/context token
/// (CPython visits `ast.operator` / `ast.expr_context` nodes in `ast.walk`).
pub enum Q<'a> {
    N(AnyNodeRef<'a>),
    T(&'a str),
}

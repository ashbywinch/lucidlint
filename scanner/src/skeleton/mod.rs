//! Structural skeletons, the walk that builds one, and the similarity bar: what a function or module
//! looks like with the names and literals abstracted away.

use ruff_python_ast::{AnyNodeRef, StmtFunctionDef};

mod interner;
mod similarity;
mod types;
mod walk;

pub use interner::BigramInterner;
pub use types::{SkeletonFn, SkeletonModule, Q};

use walk::SkeletonWalk;

/// A function's structural skeleton.
pub fn fn_skeleton(f: &StmtFunctionDef) -> Vec<String> {
    SkeletonWalk::fn_skeleton(f)
}

/// The AST node's skeleton children, in CPython's `ast.iter_child_nodes` order.
pub fn skel_children<'a>(node: AnyNodeRef<'a>, queue: &mut Vec<Q<'a>>) {
    SkeletonWalk::skel_children(node, queue)
}
pub use similarity::Similarity;

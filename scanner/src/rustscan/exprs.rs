//! Expression and macro predicates: a compound assignment, a debug artefact, an
//! assert, and whether a block mentions `self` anywhere (macros included).

use syn::{BinOp, Block, Ident, Path};

pub(crate) fn is_compound_op(op: &BinOp) -> bool {
    use syn::BinOp::*;
    matches!(
        op,
        AddAssign(_)
            | SubAssign(_)
            | MulAssign(_)
            | DivAssign(_)
            | RemAssign(_)
            | BitXorAssign(_)
            | BitAndAssign(_)
            | BitOrAssign(_)
            | ShlAssign(_)
            | ShrAssign(_)
    )
}

pub(super) fn is_dbg_macro(path: &Path) -> bool {
    path.segments.last().is_some_and(|s| s.ident == "dbg")
}

pub(super) fn is_unwrap_expect(method: &Ident) -> bool {
    method == "unwrap" || method == "expect"
}

pub(super) fn is_assert_macro(path: &Path) -> bool {
    path.segments.last().is_some_and(|s| {
        matches!(
            s.ident.to_string().as_str(),
            "assert" | "assert_eq" | "assert_ne" | "debug_assert" | "debug_assert_eq" | "debug_assert_ne"
        )
    })
}

/// Does the block reference `self` anywhere (nested included)?
///
/// Token-based, not AST-based: `self` inside a macro invocation (`write!`,
/// `format!`) is invisible to the syn visitor — a Display::fmt that only
/// touches self via `write!` would look detached. The token stream sees it.
pub(super) fn block_refs_self(block: &Block) -> bool {
    use quote::ToTokens;
    tokens_contain_ident(block.to_token_stream(), "self")
}

/// Recursive: a macro invocation's tokens live in a nested group (`write!`,
/// `format!`) — only a full descent sees `self` inside them.
pub(super) fn tokens_contain_ident(stream: proc_macro2::TokenStream, ident: &str) -> bool {
    use proc_macro2::TokenTree;
    stream.into_iter().any(|tt| match tt {
        TokenTree::Ident(id) => id == ident,
        TokenTree::Group(g) => tokens_contain_ident(g.stream(), ident),
        _ => false,
    })
}

// lucidlint: ignore-file complexity the syn dispatchers are match tables over node
// kinds and operators — arm count is the AST's surface, not branching

//! Small readers over syn nodes: attributes and cfgs, diverging statements, a
//! function's returned values, its type idents, a `use` item's paths.

use syn::visit::{self, Visit};
use syn::{Attribute, BinOp, Block, Expr, Ident, Item, ItemFn, ItemImpl, ItemUse, Path, Stmt, Type, UseTree};

pub(super) fn attrs_of(item: &Item) -> &[Attribute] {
    match item {
        Item::Fn(f) => &f.attrs,
        Item::Mod(m) => &m.attrs,
        Item::Use(u) => &u.attrs,
        Item::Struct(s) => &s.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Enum(e) => &e.attrs,
        Item::Trait(t) => &t.attrs,
        Item::Const(c) => &c.attrs,
        Item::Static(s) => &s.attrs,
        Item::Type(t) => &t.attrs,
        Item::TraitAlias(t) => &t.attrs,
        Item::Macro(m) => &m.attrs,
        Item::Union(u) => &u.attrs,
        Item::ExternCrate(e) => &e.attrs,
        Item::ForeignMod(f) => &f.attrs,
        Item::Verbatim(_) => &[],
        _ => &[],
    }
}

pub(super) fn has_attr(attrs: &[Attribute], ident: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(ident))
}

pub(super) fn has_cfg_test(attrs: &[Attribute]) -> bool {
    // Check if a cfg predicate includes the `test` config option — the
    // substring approach fires on #[cfg(not(test))] and #[cfg(feature = "testing")]
    // which are NOT test-code. The correct check: the token string, split
    // by non-alphanumeric/non-underscore characters, contains the word "test"
    // AND is not disabled by a surrounding `not(test)`.
    attrs.iter().any(|a| {
        a.path().is_ident("cfg")
            && a.meta.require_list().is_ok_and(|m| {
                let token_str = m.tokens.to_string();
                let contains_not_test = token_str.contains("not(test");
                token_str
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|w| w == "test")
                    && !contains_not_test
            })
    })
}

pub(super) fn is_diverge_stmt(s: &Stmt) -> bool {
    match s {
        Stmt::Expr(e, _) => {
            matches!(e, Expr::Return(_) | Expr::Break(_) | Expr::Continue(_))
                || matches!(e, Expr::Macro(m) if is_diverging_macro(&m.mac.path))
        }
        Stmt::Macro(m) => is_diverging_macro(&m.mac.path),
        _ => false,
    }
}

pub(super) fn is_diverging_macro(path: &Path) -> bool {
    path.segments.last().is_some_and(|s| {
        matches!(
            s.ident.to_string().as_str(),
            "panic" | "unreachable" | "todo" | "unimplemented"
        )
    })
}

/// The matching vague role suffix of a name (for the finding message).
pub(super) fn suffix_of(name: &str) -> Option<&'static str> {
    crate::naming::VAGUE_SUFFIXES
        .iter()
        .find(|s| name.ends_with(**s))
        .copied()
}

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

/// The type a function's first parameter names, when that type is a concept:
/// `&ScanState` → `ScanState`, `&Expr` → `Expr`. Primitives, strings and
/// containers are plumbing, not a receiver, so they return `None`.
pub(super) fn receiver_name(ty: &Type) -> Option<String> {
    fn peel(ty: &Type) -> Option<&Type> {
        match ty {
            Type::Reference(r) => peel(&r.elem),
            Type::Paren(p) => peel(&p.elem),
            Type::Path(_) => Some(ty),
            _ => None,
        }
    }
    let Type::Path(path) = peel(ty)? else { return None };
    let segment = path.path.segments.last()?;
    let name = segment.ident.to_string();
    const PLUMBING: &[&str] = &[
        "str", "String", "usize", "isize", "u8", "u16", "u32", "u64", "u128", "i8", "i16", "i32", "i64", "i128", "f32",
        "f64", "bool", "char", "Vec", "HashMap", "HashSet", "BTreeMap", "BTreeSet", "Option", "Box", "Rc", "Arc",
        "Cow",
    ];
    if PLUMBING.contains(&name.as_str()) {
        return None;
    }
    Some(name)
}

/// The bare type ident of a static's type (peels reference/paren wrappers).
pub(super) fn static_type_ident(ty: &Type) -> String {
    match ty {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default(),
        Type::Reference(r) => static_type_ident(&r.elem),
        Type::Paren(p) => static_type_ident(&p.elem),
        _ => String::new(),
    }
}

/// The latent-class counter: NESTED FN DEFINITIONS at any depth.
///
/// Deliberately NOT closures: Python's rule counts lambdas because a Python
/// lambda is a discouraged single-expression shim — a smell. A Rust closure
/// is the idiomatic local abstraction (iterators, builders, callbacks) and
/// counting it would fire on every well-structured function. A nested `fn`
/// definition, by contrast, is named structure hiding inside another
/// function in BOTH languages — that is the class-in-disguise signal.
pub(super) fn inner_count(block: &Block) -> u32 {
    struct Counter {
        count: u32,
    }
    impl<'ast> Visit<'ast> for Counter {
        fn visit_item_fn(&mut self, f: &'ast ItemFn) {
            self.count += 1;
            visit::visit_item_fn(self, f);
        }
    }
    let mut c = Counter { count: 0 };
    c.visit_block(block);
    c.count
}

/// The type idents referenced by a parameter type — the last segment of a
/// path plus any generic-argument paths (Vec<X>, Option<X>, &X).
pub(super) fn collect_type_idents(ty: &Type, out: &mut Vec<String>) {
    match ty {
        Type::Path(tp) => {
            if let Some(seg) = tp.path.segments.last() {
                out.push(seg.ident.to_string());
                if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                    for a in &args.args {
                        if let syn::GenericArgument::Type(t) = a {
                            collect_type_idents(t, out);
                        }
                    }
                }
            }
        }
        Type::Reference(r) => collect_type_idents(&r.elem, out),
        Type::Slice(s) => collect_type_idents(&s.elem, out),
        Type::Paren(p) => collect_type_idents(&p.elem, out),
        Type::Tuple(t) => {
            for e in &t.elems {
                collect_type_idents(e, out);
            }
        }
        _ => {}
    }
}

pub(super) fn impl_self_name(i: &ItemImpl) -> Option<String> {
    if let Type::Path(tp) = i.self_ty.as_ref() {
        tp.path.segments.last().map(|s| s.ident.to_string())
    } else {
        None
    }
}

/// The values of every `return` in the block (closures excluded).
pub(super) fn return_values(block: &Block) -> Vec<&Expr> {
    struct Probe<'a> {
        values: Vec<&'a Expr>,
    }
    impl<'ast> Visit<'ast> for Probe<'ast> {
        fn visit_expr_return(&mut self, ret: &'ast syn::ExprReturn) {
            if let Some(value) = &ret.expr {
                self.values.push(value);
            }
        }
        fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {}
    }
    let mut probe = Probe { values: Vec::new() };
    probe.visit_block(block);
    probe.values
}

pub(super) fn span_lines(start: usize, end: usize) -> usize {
    end.saturating_sub(start)
}

/// `use` paths referenced by one item, raw ("crate::a::b", "super::x", ...).
/// The last segment of each path is the imported NAME — the module edge is
/// the path minus that segment, resolved cross-file.
pub(super) fn use_paths(u: &ItemUse) -> Vec<String> {
    fn walk(tree: &UseTree, prefix: &mut Vec<String>, out: &mut Vec<String>) {
        match tree {
            UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                walk(&p.tree, prefix, out);
                prefix.pop();
            }
            UseTree::Name(n) => {
                prefix.push(n.ident.to_string());
                out.push(prefix.join("::"));
                prefix.pop();
            }
            UseTree::Rename(r) => {
                prefix.push(r.ident.to_string());
                out.push(prefix.join("::"));
                prefix.pop();
            }
            UseTree::Glob(_) => {
                out.push(prefix.join("::"));
            }
            UseTree::Group(g) => {
                for item in &g.items {
                    walk(item, prefix, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut prefix: Vec<String> = Vec::new();
    if u.leading_colon.is_some() {
        prefix.push(String::new()); // ::a::b — absolute
    }
    walk(&u.tree, &mut prefix, &mut out);
    out
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

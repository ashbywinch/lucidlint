//! Type-name readers: the receiver a parameter names, the ident an `impl` block
//! is for, and every type ident inside a type.

use syn::{ItemImpl, Type};

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
    // a smart pointer is carried, not owned: `Box<State>` names State as the
    // receiver, exactly as `&State` does — the wrapper is allocation, not type
    const POINTERS: &[&str] = &["Box", "Rc", "Arc", "Cow"];
    if POINTERS.contains(&name.as_str()) {
        let inner = match &segment.arguments {
            syn::PathArguments::AngleBracketed(args) => args.args.iter().find_map(|a| match a {
                syn::GenericArgument::Type(t) => Some(t),
                _ => None,
            }),
            _ => None,
        };
        return inner.and_then(receiver_name);
    }
    const PLUMBING: &[&str] = &[
        "str", "String", "usize", "isize", "u8", "u16", "u32", "u64", "u128", "i8", "i16", "i32", "i64", "i128", "f32",
        "f64", "bool", "char", "Vec", "HashMap", "HashSet", "BTreeMap", "BTreeSet", "Option",
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

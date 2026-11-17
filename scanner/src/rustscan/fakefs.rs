//! `fakefs` for Rust: a test drives the real filesystem through a literal path
//! — its result depends on the machine, and its writes outlive the test.

use syn::{Expr, ExprCall, Lit};

/// std::fs::File / fs::* operations — the fake-filesystem test family.
const FS_TEST_OPS: [&str; 14] = [
    "create",
    "open",
    "write",
    "append",
    "read",
    "read_to_string",
    "read_dir",
    "remove_file",
    "remove_dir",
    "remove_dir_all",
    "rename",
    "copy",
    "create_dir",
    "create_dir_all",
];

/// The op name + container of a path-call callee: (std::fs::write ->
/// ("write", "fs")), (File::create -> ("create", "File")). Empty when the
/// callee is not an fs-path call.
fn fs_path_call(c: &ExprCall) -> Option<(String, String)> {
    let Expr::Path(p) = c.func.as_ref() else {
        return None;
    };
    let segs: Vec<syn::Ident> = p.path.segments.iter().map(|s| s.ident.clone()).collect();
    let op = segs.last().map(|i| i.to_string()).unwrap_or_default();
    if !FS_TEST_OPS.contains(&op.as_str()) {
        return None;
    }
    let container = segs
        .get(segs.len().saturating_sub(2))
        .map(|i| i.to_string())
        .unwrap_or_default();
    if container == "fs" || container == "File" {
        Some((op, container))
    } else {
        None
    }
}

/// Any string-literal argument that is a relative non-temp path.
fn lit_path_arg(args: &syn::punctuated::Punctuated<Expr, syn::token::Comma>) -> bool {
    for arg in args {
        if let Expr::Lit(l) = arg {
            if let Lit::Str(s) = &l.lit {
                let p = s.value();
                if !p.starts_with('/') && !p.to_lowercase().contains("temp") {
                    return true;
                }
            }
        }
    }
    false
}

/// Does this expression touch the real filesystem with a literal path?
/// (test-code branch only)
pub(super) fn fakefs_hit(expr: &Expr) -> bool {
    match expr {
        Expr::MethodCall(mc) => {
            let op = mc.method.to_string();
            if !FS_TEST_OPS.contains(&op.as_str()) {
                return false;
            }
            matches!(
                &*mc.receiver,
                Expr::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "File" || s.ident == "fs")
            ) && lit_path_arg(&mc.args)
        }
        Expr::Call(c) => fs_path_call(c).is_some() && lit_path_arg(&c.args),
        _ => false,
    }
}

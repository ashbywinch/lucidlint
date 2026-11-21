//! The lossless text layer: a syn span is a byte range of the original source,
//! and every rewrite splices there so comments and formatting survive.

use proc_macro2::LineColumn;
use syn::spanned::Spanned;
use syn::{Item, ItemFn, Pat};

/// The lossless text layer: a syn span is a byte range of the original source.
pub struct FixText;

impl FixText {
    /// A file byte offset from a proc-macro2 LineColumn (1-based line, byte col).
    pub(super) fn byte_offset(source: &str, lc: LineColumn) -> usize {
        let (line, col) = (lc.line, lc.column);
        let mut cur = 1usize;
        for (i, b) in source.bytes().enumerate() {
            if cur == line {
                return i + col;
            }
            if b == b'\n' {
                cur += 1;
            }
        }
        source.len()
    }

    /// The fn whose span contains `line` — the loop fixes resolve by finding
    /// line, not by position in the file (a later fn's loop must not rewrite
    /// through the first fn's accumulator).
    pub(super) fn enclosing_fn(file: &syn::File, line: usize) -> Option<&ItemFn> {
        file.items.iter().find_map(|item| match item {
            Item::Fn(f) if f.span().start().line <= line && line <= f.span().end().line => Some(f),
            _ => None,
        })
    }

    pub(super) fn pat_bindings(pat: &Pat, out: &mut std::collections::HashSet<String>) {
        match pat {
            Pat::Ident(pi) => {
                out.insert(pi.ident.to_string());
            }
            Pat::Tuple(t) => t.elems.iter().for_each(|e| Self::pat_bindings(e, out)),
            Pat::Type(t) => Self::pat_bindings(&t.pat, out),
            Pat::Reference(r) => Self::pat_bindings(&r.pat, out),
            Pat::Struct(s) => s.fields.iter().for_each(|f| Self::pat_bindings(&f.pat, out)),
            Pat::Or(o) => o.cases.iter().for_each(|c| Self::pat_bindings(c, out)),
            Pat::Slice(s) => s.elems.iter().for_each(|e| Self::pat_bindings(e, out)),
            Pat::Paren(p) => Self::pat_bindings(&p.pat, out),
            Pat::TupleStruct(t) => t.elems.iter().for_each(|e| Self::pat_bindings(e, out)),
            _ => {}
        }
    }

    /// The exact source text of a span (lossless).
    pub(super) fn span_text(source: &str, span: proc_macro2::Span) -> Result<String, String> {
        let start = Self::byte_offset(source, span.start());
        let end = Self::byte_offset(source, span.end());
        Ok(source[start..end].to_string())
    }
}

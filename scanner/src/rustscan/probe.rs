//! The numeric-literal probe: a syn visitor that answers whether a subtree
//! contains a numeric literal (the magic-number rule's question).

/// G4: does the file hold a magic-CANDIDATE literal (value outside 0/1/2)
/// anywhere — the fact the stale magic-number message cites when the family
/// fires nowhere: the position rule exempted the values; the numbers are
/// not gone. The visit default reaches every expression (pattern literals
/// included) in one pass.
pub(super) struct NumericLiteralProbe {
    pub(super) found: bool,
}
impl<'ast> syn::visit::Visit<'ast> for NumericLiteralProbe {
    fn visit_expr_lit(&mut self, el: &'ast syn::ExprLit) {
        if let syn::Lit::Int(i) = &el.lit {
            if crate::magic::is_magic_value(i.base10_digits()) {
                self.found = true;
            }
        } else if let syn::Lit::Float(f) = &el.lit {
            if crate::magic::is_magic_value(f.base10_digits()) {
                self.found = true;
            }
        }
    }
}

// lucidlint: ignore-file complexity `attrs_of` is a match table over the item kinds —
// arm count is the syntax's surface, not branching

//! Attribute and cfg predicates: which item carries which attribute, and whether
//! a cfg makes it test code.

use syn::{Attribute, Item};

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

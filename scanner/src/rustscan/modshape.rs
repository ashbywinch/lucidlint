//! Module shape for Rust: what the file is (one struct, a module of parts) and
//! whether its parts belong together — the `mixed-module` family.

use syn::visit::{self, Visit};
use syn::{Attribute, File, FnArg, ImplItem, Item, Path};

use crate::finding::Finding;

use super::attrs::has_cfg_test;
use super::types::{receiver_name, static_type_ident};
use super::RustScan;

/// The Rust module shape: what the file is, and whether its parts belong together.
pub struct MixedModule;

/// `mixed-module` (#107): how many groups a module's top-level definitions fall
/// into when grouped by what they reference. A group is a connected component
/// of the item graph — the items in it reference each other (directly or
/// through a chain), and the groups never reference each other. One group is
/// one concept, however many items it holds; several groups that never meet is
/// several modules wearing one filename.
///
/// Impl blocks belong to their type (their references are the type's), `use`
/// and `mod` are boundaries rather than items, and `#[cfg(test)]` items are
/// skipped: a test module references everything, so it would glue the groups.
/// Fewer items than this never fires: below it a reader holds the parts at
/// once. The three bars are measured in
/// `docs/plans/plan-issue-107-module-shape.md` ("`mixed-module` — shipped").
pub const MIXED_MODULE_ITEMS: usize = 20;

/// Two connected groups are the normal shape of a module that has a type and
/// its helpers; three groups that never reference each other is the finding.
pub const MIXED_MODULE_GROUPS: usize = 3;

/// The fallback arm: a module large enough that even six disconnected parts
/// (groups *and* stand-alone items) cannot be one concept.
pub const MIXED_MODULE_PARTS: usize = 6;

struct Cohesion {
    items: usize,
    /// Connected components with >= 2 members, largest first.
    groups: Vec<Vec<String>>,
    /// Items in no group: they reference no other item and nothing references
    /// them. Named, not just counted — the finding says which items it saw.
    stand_alone: Vec<String>,
}

/// The paths an item uses, restricted to the module's own definition names.
struct RefProbe<'a> {
    known: &'a std::collections::HashSet<String>,
    found: std::collections::HashSet<String>,
}

impl<'ast> Visit<'ast> for RefProbe<'_> {
    fn visit_path(&mut self, path: &'ast Path) {
        if let Some(first) = path.segments.first() {
            let ident = first.ident.to_string();
            if self.known.contains(&ident) {
                self.found.insert(ident);
            }
        }
        visit::visit_path(self, path);
    }
}

impl MixedModule {
    /// The file's shape for the module-shape rules (#107): the types, the free
    /// `fn`s that take one of them (a group is a class equivalent), and the free
    /// functions left with no receiver. An `impl` block is not a definition of its
    /// own — its methods are the type's; `const`/`static` are data, and an inline
    /// `mod` is a container.
    pub(crate) fn module_shape(file: &File, source: &str) -> crate::checks::ModuleShape {
        let mut types = Self::declared_types(file);
        Self::attach_impl_methods(file, &mut types);
        let local: std::collections::HashSet<&str> = types.iter().map(|t| t.name.as_str()).collect();
        let (free, grouped) = Self::foreign_receivers(file, &local);
        crate::checks::ModuleShape::rust(source, free, types, grouped)
    }

    /// The module's own types, each with the method count it declares itself.
    fn declared_types(file: &File) -> Vec<crate::checks::TypeShape> {
        use crate::checks::TypeShape;
        let mut types: Vec<TypeShape> = Vec::new();
        for item in &file.items {
            match item {
                Item::Struct(i) => types.push(TypeShape {
                    name: i.ident.to_string(),
                    methods: 0,
                }),
                Item::Enum(i) => types.push(TypeShape {
                    name: i.ident.to_string(),
                    methods: 0,
                }),
                Item::Union(i) => types.push(TypeShape {
                    name: i.ident.to_string(),
                    methods: 0,
                }),
                Item::Type(i) => types.push(TypeShape {
                    name: i.ident.to_string(),
                    methods: 0,
                }),
                Item::Trait(i) => types.push(TypeShape {
                    name: i.ident.to_string(),
                    methods: i.items.iter().filter(|t| matches!(t, syn::TraitItem::Fn(_))).count(),
                }),
                _ => {}
            }
        }
        types
    }

    /// The methods each `impl` block adds to its type.
    fn attach_impl_methods(file: &File, types: &mut [crate::checks::TypeShape]) {
        for item in &file.items {
            let Item::Impl(block) = item else { continue };
            let name = static_type_ident(&block.self_ty);
            let methods = block.items.iter().filter(|i| matches!(i, ImplItem::Fn(_))).count();
            if let Some(shape) = types.iter_mut().find(|t| t.name == name) {
                shape.methods += methods;
            }
        }
    }

    /// Free functions grouped by the type their first parameter names — a function
    /// that takes one of the module's own types is that type's; one that takes a
    /// foreign type (`&Expr`, `&Path`) joins that type's group; a function with no
    /// named receiver is free. Returns (free count, groups by weight).
    fn foreign_receivers(file: &File, local: &std::collections::HashSet<&str>) -> (usize, Vec<(String, usize)>) {
        let mut groups: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        let mut free = 0usize;
        for item in &file.items {
            let Item::Fn(function) = item else { continue };
            let receiver = function.sig.inputs.first().and_then(|arg| match arg {
                FnArg::Receiver(_) => None,
                FnArg::Typed(typed) => receiver_name(&typed.ty),
            });
            match receiver {
                Some(name) if local.contains(name.as_str()) => {}
                Some(name) => *groups.entry(name).or_default() += 1,
                None => free += 1,
            }
        }
        let mut grouped: Vec<(String, usize)> = groups.into_iter().collect();
        grouped.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        (free, grouped)
    }

    fn cohesion(file: &File) -> Cohesion {
        let names = Self::definition_names(file);
        let known: std::collections::HashSet<String> = names.iter().cloned().collect();
        let index: std::collections::HashMap<&str, usize> =
            names.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();
        let parent = Self::reference_groups(file, &known, &index);
        let (groups, stand_alone) = Self::components(&names, parent);
        Cohesion {
            items: names.len(),
            groups,
            stand_alone,
        }
    }

    /// The module's definitions: functions, types, constants; an impl block's
    /// references belong to the type it implements.
    fn definition_names(file: &File) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for item in &file.items {
            if has_cfg_test(Self::item_attrs(item)) {
                continue;
            }
            let name = match item {
                Item::Fn(i) => Some(i.sig.ident.to_string()),
                Item::Struct(i) => Some(i.ident.to_string()),
                Item::Enum(i) => Some(i.ident.to_string()),
                Item::Union(i) => Some(i.ident.to_string()),
                Item::Type(i) => Some(i.ident.to_string()),
                Item::Trait(i) => Some(i.ident.to_string()),
                Item::Const(i) => Some(i.ident.to_string()),
                Item::Static(i) => Some(i.ident.to_string()),
                Item::Impl(i) => Some(static_type_ident(&i.self_ty)),
                _ => None,
            };
            if let Some(name) = name.filter(|n| !n.is_empty()) {
                names.push(name);
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// One set per definition, merged wherever an item references another item in
    /// this module — the union-find the component count reads.
    fn reference_groups(
        file: &File,
        known: &std::collections::HashSet<String>,
        index: &std::collections::HashMap<&str, usize>,
    ) -> Vec<usize> {
        let mut parent: Vec<usize> = (0..known.len()).collect();
        fn find(parent: &mut [usize], mut x: usize) -> usize {
            while parent[x] != x {
                parent[x] = parent[parent[x]];
                x = parent[x];
            }
            x
        }
        for item in &file.items {
            if has_cfg_test(Self::item_attrs(item)) {
                continue;
            }
            let owner = match item {
                Item::Impl(i) => static_type_ident(&i.self_ty),
                _ => Self::item_name(item).unwrap_or_default(),
            };
            if owner.is_empty() {
                continue;
            }
            let mut probe = RefProbe {
                known,
                found: std::collections::HashSet::new(),
            };
            probe.visit_item(item);
            let Some(&owner_idx) = index.get(owner.as_str()) else {
                continue;
            };
            for referenced in &probe.found {
                if *referenced == owner {
                    continue;
                }
                if let Some(&other) = index.get(referenced.as_str()) {
                    let (a, b) = (find(&mut parent, owner_idx), find(&mut parent, other));
                    if a != b {
                        parent[b] = a;
                    }
                }
            }
        }
        parent
    }

    /// The connected groups of two or more definitions (largest first), and the
    /// definitions that stand outside every group.
    fn components(names: &[String], mut parent: Vec<usize>) -> (Vec<Vec<String>>, Vec<String>) {
        fn find(parent: &mut [usize], mut x: usize) -> usize {
            while parent[x] != x {
                parent[x] = parent[parent[x]];
                x = parent[x];
            }
            x
        }
        let mut by_root: std::collections::HashMap<usize, Vec<String>> = std::collections::HashMap::new();
        for (i, name) in names.iter().enumerate() {
            let root = find(&mut parent, i);
            by_root.entry(root).or_default().push(name.clone());
        }
        let (mut groups, islands): (Vec<Vec<String>>, Vec<Vec<String>>) = by_root
            .into_values()
            .map(|mut members| {
                members.sort();
                members
            })
            .partition(|members| members.len() >= 2);
        let mut stand_alone: Vec<String> = islands.into_iter().flatten().collect();
        stand_alone.sort();
        groups.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].cmp(&b[0])));
        (groups, stand_alone)
    }

    fn item_attrs(item: &Item) -> &[Attribute] {
        match item {
            Item::Fn(i) => &i.attrs,
            Item::Struct(i) => &i.attrs,
            Item::Enum(i) => &i.attrs,
            Item::Union(i) => &i.attrs,
            Item::Type(i) => &i.attrs,
            Item::Trait(i) => &i.attrs,
            Item::Const(i) => &i.attrs,
            Item::Static(i) => &i.attrs,
            Item::Impl(i) => &i.attrs,
            Item::Mod(i) => &i.attrs,
            Item::Use(i) => &i.attrs,
            _ => &[],
        }
    }

    fn item_name(item: &Item) -> Option<String> {
        match item {
            Item::Fn(i) => Some(i.sig.ident.to_string()),
            Item::Struct(i) => Some(i.ident.to_string()),
            Item::Enum(i) => Some(i.ident.to_string()),
            Item::Union(i) => Some(i.ident.to_string()),
            Item::Type(i) => Some(i.ident.to_string()),
            Item::Trait(i) => Some(i.ident.to_string()),
            Item::Const(i) => Some(i.ident.to_string()),
            Item::Static(i) => Some(i.ident.to_string()),
            Item::Impl(i) => Some(static_type_ident(&i.self_ty)),
            _ => None,
        }
    }

    /// Emit the `mixed-module` finding into a scan: skipped for generated files and
    /// test paths (a generated module has no reader to organise for, and a test
    /// module is not the module's design).
    pub(crate) fn mixed_module_into(scan: &mut RustScan, file: &File, name: &str) {
        if scan.module_shape.generated || crate::is_test_path(name) {
            return;
        }
        if let Some(finding) = Self::mixed_module_finding(file, name) {
            scan.findings.push(finding);
        }
    }

    /// One finding when the module's definitions fall into groups that never
    /// reference each other.
    fn mixed_module_finding(file: &File, name: &str) -> Option<Finding> {
        let shape = Self::cohesion(file);
        // Two arms: three families that never meet, or a big module whose items
        // fall into many disconnected parts (the grab bag — several small groups
        // and a pile of stand-alone items, which no single family size reveals).
        let parts = shape.groups.len() + shape.stand_alone.len();
        let several_families = shape.groups.len() >= MIXED_MODULE_GROUPS;
        let fragmented = shape.items >= MIXED_MODULE_ITEMS && parts >= MIXED_MODULE_PARTS;
        if !several_families && !fragmented {
            return None;
        }
        // Name what the file holds, whichever shape it is: the groups by their
        // members, or — when every item is an island — that there are no groups at
        // all. A sentence that says "the groups are ." over an empty list is the
        // confusion this message exists to remove (2026-11-18 review).
        let detail = if shape.groups.is_empty() {
            format!(
                "None of them references another: {} items standing alone, so the file names no families at all. A module holds one concern, and a concern is a type with its impl. Group them into modules by what they operate on, and let each module's type own the functions that take it.",
                shape.stand_alone.len()
            )
        } else {
            let list = shape
                .groups
                .iter()
                .take(3)
                .map(|g| match g.len() {
                    1 => g[0].clone(),
                    2 => format!("{} and {}", g[0], g[1]),
                    n => format!("{} and {} others", g[0], n - 1),
                })
                .collect::<Vec<_>>()
                .join("; ");
            let islands = match shape.stand_alone.len() {
                0 => String::new(),
                n => format!(" and {n} items standing alone"),
            };
            format!(
                "{} groups{islands}. The groups are {list}. A module holds one concern, and a concern is a type with its impl. Move each group into its own module, named after what its members are about.",
                shape.groups.len(),
            )
        };
        Some(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col: 0,
            file: name.to_string(),
            line: 0,
            function: String::new(),
            kind: "mixed-module".into(),
            severity: "warn".into(),
            message: format!(
                "'{name}' holds {} top-level definitions in {parts} parts that never reference each other. A reader cannot tell which items belong together, because nothing in the file joins one part to the next. {detail}",
                shape.items,
            ),
        })
    }
}

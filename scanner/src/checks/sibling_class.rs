//! The value-class record the sibling rule compares: its fields, bases and methods.

use crate::finding::Finding;

/// One method's shape for the sibling pass: the body's skeleton (compared with
/// the duplicate machinery), the `self.<x>` attributes it reads, and the
/// annotation names in its signature (a pair that mentions the other class is
/// composition, not duplication).
#[derive(Clone)]
pub struct SiblingMethod {
    pub name: String,
    pub skeleton: Vec<String>,
    pub uses: Vec<String>,
    pub mentions: Vec<String>,
}

/// One class's shape for the sibling pass: the instance fields it holds and
/// what its methods read — enough to compare two value classes without
/// borrowing the file they came from (one class per file is the house
/// convention, so a same-file-only rule would barely fire).
#[derive(Clone)]
pub struct SiblingClass {
    pub name: String,
    pub line: usize,
    /// instance field names: class-body annotations + `self.x =` assignments
    pub fields: Vec<String>,
    /// annotation base names of those fields, when present
    pub field_types: Vec<String>,
    pub bases: Vec<String>,
    pub methods: Vec<SiblingMethod>,
}

impl SiblingClass {
    /// The fields both classes hold. One generic name is coincidence; two is
    /// the same value seen twice.
    fn shared_fields(&self, other: &Self) -> Vec<&str> {
        self.fields
            .iter()
            .filter(|f| other.fields.contains(f))
            .map(|f| f.as_str())
            .collect()
    }

    /// Framework bases that do not make two classes variants of one DOMAIN
    /// abstraction — a shared pydantic/Protocol base still leaves two duplicate
    /// value classes to reconcile.
    const FRAMEWORK_BASES: &'static [&'static str] = &[
        "object",
        "BaseModel",
        "Protocol",
        "ABC",
        "ABCMeta",
        "NamedTuple",
        "TypedDict",
        "Generic",
        "Enum",
        "IntEnum",
        "StrEnum",
        "Flag",
        "Exception",
    ];

    /// Two operators of one formula language, two node kinds of one pipeline:
    /// they share a domain base, so the shared half already has a home — the
    /// base. Reporting them as duplicates invites merging variants into one
    /// class with a flag, which erases the distinction they exist for.
    fn shares_domain_base(&self, other: &Self) -> bool {
        self.bases
            .iter()
            .any(|b| other.bases.contains(b) && !Self::FRAMEWORK_BASES.contains(&b.as_str()))
    }

    /// How the two classes are joined — containment, inheritance, a shared
    /// domain base, or a signature that names the other class. Composition,
    /// not duplication: the containment case belongs to `duplicate-field`.
    fn composed_with(&self, other: &Self) -> bool {
        self.bases.contains(&other.name)
            || other.bases.contains(&self.name)
            || self.shares_domain_base(other)
            || self.field_types.contains(&other.name)
            || other.field_types.contains(&self.name)
            || self.methods.iter().any(|m| m.mentions.contains(&other.name))
            || other.methods.iter().any(|m| m.mentions.contains(&self.name))
    }

    /// The accessors this class derives from `shared`, structurally equal
    /// (dice >= 0.9) to one on `other` that reads the same fields.
    fn matching_accessors<'a>(&'a self, other: &'a Self, shared: &[&str]) -> Vec<&'a str> {
        let shared_set: std::collections::HashSet<&str> = shared.iter().copied().collect();
        let reads_only_shared = |m: &&SiblingMethod| {
            !m.name.starts_with("__") && !m.uses.is_empty() && m.uses.iter().all(|u| shared_set.contains(u.as_str()))
        };
        self.methods
            .iter()
            .filter(reads_only_shared)
            .filter(|m| {
                other
                    .methods
                    .iter()
                    .filter(reads_only_shared)
                    .any(|n| crate::skeleton::Similarity::dice_similarity(&m.skeleton, &n.skeleton) >= 0.9)
            })
            .map(|m| m.name.as_str())
            .collect()
    }

    /// The finding when `other` is this class's duplicate — the later class in
    /// scan order is the likely duplicate; the earlier one is what the
    /// codebase already reaches for.
    pub(super) fn duplicate_of(&self, other_rel: &str, other: &Self) -> Option<Finding> {
        let shared = self.shared_fields(other);
        if shared.len() < 2 || self.composed_with(other) {
            return None;
        }
        let accessors = self.matching_accessors(other, &shared);
        if accessors.len() < 2 {
            return None;
        }
        let shown = accessors.iter().take(3).copied().collect::<Vec<_>>().join(", ");
        Some(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col: 0,
            file: other_rel.to_string(),
            line: other.line,
            function: format!("{}, {}", self.name, other.name),
            kind: "sibling-value-class".into(),
            severity: "warn".into(),
            message: format!(
                "'{}' and '{}' both hold {} and derive {} the same way, with no containment — the same data has two homes. If they are variants of one thing, give them the base that owns the shared half; if they are the same value, keep one and move the other's extra accessors onto it. Decide which before editing: merging two variants erases the distinction they exist for.",
                self.name,
                other.name,
                shared.join(", "),
                shown
            ),
        })
    }
}

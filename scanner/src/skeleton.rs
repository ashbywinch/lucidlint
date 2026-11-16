//! Structural skeletons and the similarity bar: what a function or module
//! looks like with the names and literals abstracted away.

/// One duplicate candidate: the file/name/line plus its structural skeleton.
/// The skeleton is a language-layer product (ruff op tokens for Python, syn
/// tokens for Rust); the similarity machinery below is language-neutral.
#[derive(Clone)]
pub struct SkeletonFn {
    pub rel: String,
    pub name: String,
    pub line: usize,
    /// last line of the definition — a function nested in another always sits
    /// inside its parent's span, which is containment, not copy-paste
    pub end_line: usize,
    pub skeleton: Vec<String>,
}

/// One duplicate-module candidate (#26): a module or top-level class
/// identity with its structural skeleton, constant tokens, and member-name
/// set. Built per file in the Python layer; the repo-wide pass pairs
/// identities with >= 0.9 Dice AND identical constant sets.
#[derive(Clone)]
pub struct SkeletonModule {
    pub rel: String,
    pub name: String,
    pub line: usize,
    pub skeleton: Vec<String>,
    /// `NAME:norm(type)` tokens for the identity's `NAME = literal` constants
    /// (numeric literals normalised, strings stripped, bools canonical).
    pub consts: Vec<String>,
    /// Sorted member names — the issue's "matched members:" intersection.
    pub members: Vec<String>,
    /// Sorted declared field names — class-body annotations plus the
    /// `self.<name> = ...` assignments in methods. A record's fields are its
    /// facts: two identities whose declared fields differ are siblings by
    /// shape, not copies (#73).
    pub fields: Vec<String>,
    /// "module" | "class" | "module-class" (a module whose non-import
    /// top-levels are exactly one class def — pairs as both).
    pub entity: &'static str,
    /// The class identity's direct base names ("Name:Base" / "Attr:mod:Base")
    /// — empty for module/module-class identities. C2: two classes sharing
    /// one base get the subclass message, not the fork prose.
    pub bases: Vec<String>,
}

/// A function is a duplicate candidate when it has real body substance:
/// at least 2 non-doc statements and a skeleton of at least 12 tokens.
/// Pure; each layer supplies its own statement count (docstring filtering
/// is Python-shaped).
pub fn is_duplicate_size(skeleton_len: usize, non_doc_stmts: usize) -> bool {
    non_doc_stmts >= 2 && skeleton_len >= 12
}

/// Dice coefficient over bigram sets — the language-neutral similarity.
/// MULTISET semantics (pinned by `duplicate_dice_contract_no_set_hash_shortcut`):
/// repeated bigrams count. Computed via count maps instead of the old
/// repo cost 69ms of the LSP's save-time merge; this is O(len(a) + len(b)).
/// Reference implementation only now — production scores via the sorted-
/// bigram fast path in `duplicate_findings`; the parity tests pin them equal.
/// NOT `#[cfg(test)]`-gated: gen-rules.py treats the first cfg(test) as the
/// test-module boundary and would miss the emissions later in this file.
#[allow(dead_code)]
pub fn dice_similarity(a: &[String], b: &[String]) -> f64 {
    let bigram_counts = |t: &[String]| -> std::collections::HashMap<(String, String), usize> {
        let mut m = std::collections::HashMap::new();
        for w in t.windows(2) {
            *m.entry((w[0].clone(), w[1].clone())).or_insert(0) += 1;
        }
        m
    };
    let (ab, bb) = (bigram_counts(a), bigram_counts(b));
    if ab.is_empty() && bb.is_empty() {
        return 0.0; // matches the Python reference: no shared bigrams, never a duplicate
    }
    if ab.is_empty() || bb.is_empty() {
        return 0.0;
    }
    let mut common = 0usize;
    for (k, ca) in &ab {
        if let Some(cb) = bb.get(k) {
            common += ca.min(cb);
        }
    }
    let total_a: usize = ab.values().sum();
    let total_b: usize = bb.values().sum();
    (2.0 * common as f64) / (total_a + total_b) as f64
}

// --------------------------------------------------------------------------- suppressions

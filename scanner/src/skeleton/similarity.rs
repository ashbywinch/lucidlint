//! The similarity bar: when two skeletons are duplicate candidates, and how
//! close their bigram Dice scores are.

use ruff_python_ast::{Expr, Stmt, StmtFunctionDef};

/// The similarity rules — the size gate, the Dice score and its interned form.
/// The skeleton length below which a comparison is noise, and the Dice pair
/// weight (two shared items over the total).
const SKELETON_LEN_MIN: usize = 12;
const DICE_PAIR_WEIGHT: f64 = 2.0;

pub struct Similarity;

impl Similarity {
    /// A function is a duplicate candidate when it has real body substance:
    /// at least 2 non-doc statements and a skeleton of at least 12 tokens.
    /// Pure; each layer supplies its own statement count (docstring filtering
    /// is Python-shaped).
    pub fn is_duplicate_size(skeleton_len: usize, non_doc_stmts: usize) -> bool {
        non_doc_stmts >= 2 && skeleton_len >= SKELETON_LEN_MIN
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
        (DICE_PAIR_WEIGHT * common as f64) / (total_a + total_b) as f64
    }
    /// `_is_duplicate_candidate`: at least two real statements (one-line
    /// accessors are not copy-paste) and a 12+ token skeleton; `__init__`
    /// boilerplate is conventional.
    pub fn is_duplicate_candidate(f: &StmtFunctionDef, skeleton_len: usize) -> bool {
        if f.name.as_str() == "__init__" {
            return false;
        }
        let stmts: Vec<&Stmt> = f
            .body
            .iter()
            .filter(|s| !matches!(s, Stmt::Expr(e) if matches!(e.value.as_ref(), Expr::StringLiteral(_))))
            .collect();
        stmts.len() >= 2 && skeleton_len >= SKELETON_LEN_MIN
    }
    /// Dice over SORTED bigram lists — multiset intersection via a two-pointer
    /// merge (counts equal runs). Matches `dice_similarity`'s multiset contract
    /// exactly; the pairing test pins them equal on the review-log cases.
    pub fn dice_from_bigrams(a: &[(u32, u32)], b: &[(u32, u32)]) -> f64 {
        if a.is_empty() && b.is_empty() {
            return 0.0;
        }
        if a.is_empty() || b.is_empty() {
            return 0.0;
        }
        let mut common = 0usize;
        let (mut i, mut j) = (0usize, 0usize);
        while i < a.len() && j < b.len() {
            if a[i] == b[j] {
                let x = a[i];
                let (ci, cj) = (i, j);
                while i < a.len() && a[i] == x {
                    i += 1;
                }
                while j < b.len() && b[j] == x {
                    j += 1;
                }
                common += (i - ci).min(j - cj);
            } else if a[i] < b[j] {
                i += 1;
            } else {
                j += 1;
            }
        }
        (DICE_PAIR_WEIGHT * common as f64) / (a.len() + b.len()) as f64
    }
}

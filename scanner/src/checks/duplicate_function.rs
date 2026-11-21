//! `duplicate`: two functions whose structural skeletons match at or above the
//! similarity bar — a copy-paste fork that one edit can leave behind.

use rayon::prelude::*;

use crate::finding::Finding;
use crate::skeleton::{BigramInterner, Similarity, SkeletonFn};

/// Cross-file copy-paste findings (`_duplicate_actions`).
///
/// Not O(n²): the tolerance guard (`|len_a - len_b| <= max(2, len/5)`) means
/// only skeletons of similar length can ever match, so candidates are
/// bucketed by skeleton length and each one probes only the lengths within
/// its window — near-linear for real repos, and the all-same-length
/// degenerate case is genuinely pairwise (every candidate is a valid peer).
/// `_first_duplicate` semantics are preserved exactly: the earliest LATER
/// candidate (list order) at >= 90% Dice.
///
/// Dice is computed for every pair: a content hash of the unique bigram SET
/// cannot shortcut the decision — equal unique sets with different
/// multiplicities (a repeated bigram) score Dice < 0.9 while hashing equal,
/// and differing sets can still score >= 0.9. The set hash is neither
/// necessary nor sufficient, so it is not a valid collision test
/// (2026-08-17 review-log: set-vs-multiset 100%-similar false positive).
/// The length bucket size for pair comparison, and the Dice similarity at
/// which two skeletons are the same function.
const LENGTH_BUCKET_DIVISOR: usize = 5;
const DICE_MIN: f64 = 0.9;

pub fn duplicate_findings(fns: &[SkeletonFn]) -> Vec<Finding> {
    use std::collections::HashMap;
    let mut out = Vec::new();
    let interner = BigramInterner::new(&fns.iter().map(|f| f.skeleton.as_slice()).collect::<Vec<_>>());
    let mut buckets: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, fr) in fns.iter().enumerate() {
        buckets.entry(fr.skeleton.len()).or_default().push(i);
    }
    let mut len_keys: Vec<usize> = buckets.keys().copied().collect();
    len_keys.sort_unstable();
    let results: Vec<Option<Finding>> = (0..fns.len())
        .into_par_iter()
        .map(|i| {
            let fr = &fns[i];
            let l = fr.skeleton.len();
            let tol = (l / LENGTH_BUCKET_DIVISOR).max(2);
            let lo = l.saturating_sub(tol);
            let hi = l + tol;
            let mut best: Option<(usize, f64)> = None;
            for &len in len_keys.iter().filter(|&&k| k >= lo && k <= hi) {
                let bucket = &buckets[&len];
                let start = bucket.partition_point(|&j| j <= i);
                for &j in &bucket[start..] {
                    let sim = Similarity::dice_from_bigrams(interner.bigrams_of(i), interner.bigrams_of(j));
                    if sim >= DICE_MIN {
                        // a function nested inside another always resembles it:
                        // the span contains the other's — containment, not
                        // copy-paste (houses: `_fetch` inside `api_fetch`)
                        // line numbers are per file: only a SAME-FILE pair can
                        // be nested (two files' copies share the same numbers)
                        let one = (fr.line, fr.end_line);
                        let two = (fns[j].line, fns[j].end_line);
                        let nested = fr.rel == fns[j].rel
                            && ((one.0 <= two.0 && two.1 <= one.1) || (two.0 <= one.0 && one.1 <= two.1));
                        if nested {
                            continue;
                        }
                        if best.is_none_or(|(b, _)| j < b) {
                            best = Some((j, sim));
                        }
                        break; // later members of this bucket are later indices
                    }
                }
            }
            if let Some((j, sim)) = best {
                let dup = &fns[j];
                return Some(Finding { logical_start: None, seam_members: Vec::new(),
                col: 0,
                file: dup.rel.clone(),
                line: dup.line,
                function: dup.name.clone(),
                kind: "duplicate".into(),
                severity: "warn".into(),
                message: format!(
                    "function '{}' ({}:{}) is {:.0}% similar to '{}' ({}:{}) — copy-paste; extract the shared logic into one function",
                    dup.name, dup.rel, dup.line, sim * 100.0, fr.name, fr.rel, fr.line
                ), });
            }
            None
        })
        .collect();
    out.extend(results.into_iter().flatten());
    out
}

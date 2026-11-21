//! The bigram interner: every skeleton's sorted bigram list, interned once for
//! a repo-wide pass.

/// Interns skeleton tokens to small ids and precomputes each function's
/// sorted bigram index list ONCE — `duplicate_findings` then scores pairs
/// with an allocation-free two-pointer merge instead of rebuilding bigram
/// maps per pair (the save-time repo-wide merge's dominant cost: 59ms on a
/// 57-file repo for 224 skeletons).
pub struct BigramInterner {
    bigrams: Vec<(u32, u32)>,
    offsets: Vec<(usize, usize)>, // (start, end) into bigrams per input
}
impl BigramInterner {
    pub fn new(skeletons: &[&[String]]) -> Self {
        let mut intern: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
        let mut bigrams: Vec<(u32, u32)> = Vec::new();
        let mut offsets: Vec<(usize, usize)> = Vec::with_capacity(skeletons.len());
        for skel in skeletons {
            let mut ids: Vec<u32> = Vec::with_capacity(skel.len());
            for tok in *skel {
                let next = intern.len() as u32;
                let id = *intern.entry(tok.as_str()).or_insert(next);
                ids.push(id);
            }
            let start = bigrams.len();
            bigrams.extend(ids.windows(2).map(|w| (w[0], w[1])));
            bigrams[start..].sort_unstable();
            offsets.push((start, bigrams.len()));
        }
        Self { bigrams, offsets }
    }

    /// The function's sorted bigram list.
    pub fn bigrams_of(&self, i: usize) -> &[(u32, u32)] {
        let (s, e) = self.offsets[i];
        &self.bigrams[s..e]
    }
}

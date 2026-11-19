//! The line-marker binder: which members one marker covers, inner-first.

use super::Suppressions;

pub(super) struct LineMarkerCtx<'a> {
    pub(super) supps: &'a Suppressions,
    pub(super) used_line: &'a mut std::collections::HashSet<(usize, String)>,
    pub(super) taken: &'a mut std::collections::HashSet<(usize, String)>,
    pub(super) spent: &'a mut std::collections::HashSet<(usize, String)>,
}
/// Bind one line-group's members to its markers inner-first. Returns flags
/// parallel to `members`: true = exempted by a LINE marker (consumed).
/// G3: one marker binds EVERY member anchored at the same col — several
/// record-shaped params of one def all report at col 0, and a marker above
/// the def must cover the whole site. Nested findings at DISTINCT cols
/// (inner literals) keep the innermost-first rule: one marker peels only
/// the innermost record; two peel inner then outer.
impl<'a> LineMarkerCtx<'a> {
    /// Bind one line-group's members to its markers inner-first.
    pub(super) fn peel(&mut self, members: &[usize], findings: &[crate::Finding]) -> Vec<bool> {
        let pairs = Suppressions::marker_inventory(members, findings, self.supps);
        let mut ok = vec![false; members.len()];
        // members sort by col DESC (inner-first) while the inventory sorts
        // by line DESC — group members by col so one marker serves one
        // anchor.
        let mut used: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut by_col: Vec<(usize, Vec<usize>)> = Vec::new();
        for (j, &i) in members.iter().enumerate() {
            match by_col.last_mut() {
                Some((c, js)) if *c == findings[i].col => js.push(j),
                _ => by_col.push((findings[i].col, vec![j])),
            }
        }
        for (_col, js) in by_col {
            let matches = |pi: usize, j: usize| Suppressions::signal_matches(&pairs[pi].1, &findings[members[j]].kind);
            // the nearest unused marker whose signal names this site's members
            if let Some(pi) = (0..pairs.len()).find(|&pi| !used.contains(&pi) && js.iter().any(|&j| matches(pi, j))) {
                // G3: one marker SITE (one comment line) binds every member
                // anchored at this col that the line's signals name. A
                // comma-named line (`ignore closures,large-function`) covers
                // both; a `closures` marker must not silence the large-function
                // reported at the same anchor. Nested sites at distinct cols
                // still peel innermost-first, one marker line per site.
                let marker_line = pairs[pi].0;
                let applied: Vec<usize> = (0..pairs.len())
                    .filter(|&qi| {
                        !used.contains(&qi) && pairs[qi].0 == marker_line && js.iter().any(|&j| matches(qi, j))
                    })
                    .collect();
                for &qi in &applied {
                    used.insert(qi);
                    let cand = pairs[qi].clone();
                    self.taken.insert(cand.clone());
                    self.used_line.insert(cand.clone());
                    self.spent.insert(cand);
                }
                for &j in &js {
                    if applied.iter().any(|&qi| matches(qi, j)) {
                        ok[j] = true;
                    }
                }
            }
        }
        ok
    }
}

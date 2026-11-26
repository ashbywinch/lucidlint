//! The stale-marker check: is a comment's claimed signal still bound?

use super::Suppressions;

/// The stale-check context — the filter state + comment stream of one file.
pub(super) struct StaleCtx<'a> {
    pub(super) supps: &'a Suppressions,
    pub(super) used_line: &'a std::collections::HashSet<(usize, String)>,
    pub(super) used_file: &'a std::collections::HashSet<String>,
    pub(super) comments: &'a [(usize, String)],
    pub(super) file: &'a str,
    pub(super) marker: &'a str,
    /// Every finding's (kind, lines) that fired in this scan — the facts
    /// the stale binding-reason reasons cite (gone / covered / out of
    /// window).
    pub(super) by_signal: &'a std::collections::HashMap<String, Vec<usize>>,
    /// G4: the exemption-kind labels among the file's still-present
    /// magic-number candidates (unit-named, constant-definition,
    /// data-table, length-guard, position/index) — the stale message names
    /// exactly the kinds that cover the file's literals, never a blanket
    /// unit/constant/table claim for a file whose literals are indices.
    pub(super) magic_exempt_labels: &'a [&'static str],
}
impl StaleCtx<'_> {
    /// G4: the rule-delta clause for a stale marker. The magic-number
    /// family's current exemptions (unit-named values, named constants,
    /// data tables, length guards, the position/index skip) can cover a
    /// file's literals — then the family fires nowhere although the numbers
    /// are still there. The stale message names ONLY the kinds present among
    /// the file's exempted literals so the reader can verify before deleting
    /// the marker; every other kind keeps the plain text.
    fn exemption_delta(&self, sig: &str) -> String {
        if sig != "magic-number" || self.magic_exempt_labels.is_empty() {
            return String::new();
        }
        let labels = self.magic_exempt_labels;
        let joined = match labels.len() {
            1 => labels[0].to_string(),
            2 => format!("{} and {}", labels[0], labels[1]),
            _ => {
                let mut s = String::new();
                for (i, l) in labels.iter().enumerate() {
                    if i + 2 == labels.len() {
                        s.push_str(&format!("{l}, and "));
                    } else if i == labels.len() - 1 {
                        s.push_str(l);
                    } else {
                        s.push_str(&format!("{l}, "));
                    }
                }
                s
            }
        };
        let noun = if labels.len() == 1 { "exemption" } else { "exemptions" };
        format!(
        " — the file still holds numeric literals — now covered by the {joined} {noun}; verify the reason before deleting the marker"
    )
    }
}
impl<'a> StaleCtx<'a> {
    /// Stale suppressions: an explained suppression that matched nothing is
    /// dead weight — the signal it names no longer fires on that line/file
    /// (a family renamed, a finding fixed, a comment moved). Why-less ones
    /// are already findings; they never match by design.
    pub(super) fn stale_suppression_findings(&self) -> Vec<crate::Finding> {
        let mut out = Vec::new();
        // The `— fix: stale-suppression` directive goes only where the fix
        // command can actually delete the marker (the Python engine). A Rust
        // file's stale marker has no fixer; an advertised fix that cannot
        // run is a false suggestion (user ruling) — "remove it" IS the fix.
        let fix_tail = if self.file.ends_with(".rs") {
            ""
        } else {
            " — fix: stale-suppression"
        };
        for (ln, entries) in &self.supps.line {
            for (sig, why) in entries {
                if why.is_empty() || self.used_line.contains(&(*ln, sig.clone())) {
                    continue;
                }
                // "stale — remove it" means exactly one case: the signal
                // fires NOWHERE in this file. When the family still fires
                // anywhere, the marker is documentation (a mis-placed or
                // covered marker binds nothing) — no stale verdict, and no
                // advice to move it (plan Phase 3).
                let Some(reason) = self.stale_reason(sig) else {
                    continue;
                };
                let delta = self.exemption_delta(sig);
                out.push(crate::Finding { logical_start: None, seam_members: Vec::new(), col: 0,
            file: self.file.to_string(),
            line: *ln,
            function: String::new(),
            kind: "stale-suppression".into(),
            severity: "fail".into(),
            message: format!(
                "suppression '{} lucidlint: ignore {sig}' at line {ln} no longer fires ({reason}{delta}) — remove it{fix_tail}",
                self.marker
            ), });
            }
        }
        for (sig, why) in &self.supps.file {
            if why.is_empty() || self.used_file.contains(sig) {
                continue;
            }
            if let Some(ln) = super::ignore_file_comment_line(self.comments, sig) {
                // same fires-nowhere gate as the line markers: a file
                // suppression over a family that still fires is either bound
                // or redundant documentation — never a stale verdict
                if self.lines_for(sig).is_empty() {
                    let delta = self.exemption_delta(sig);
                    out.push(crate::Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                file: self.file.to_string(),
                line: ln,
                function: String::new(),
                kind: "stale-suppression".into(),
                severity: "fail".into(),
                message: format!(
                    "file suppression '{} lucidlint: ignore-file {sig}' no longer fires (no matching finding fires in this file — it was fixed, or the kind was renamed{delta}) — remove it{fix_tail}",
                    self.marker
                ), });
                }
            }
        }
        out
    }

    /// Every line where a finding matching `sig` (family-aware) fired in
    /// this scan — the facts the fires-nowhere verdict cites.
    fn lines_for(&self, sig: &str) -> Vec<usize> {
        let mut lines: Vec<usize> = self
            .by_signal
            .iter()
            .filter(|(kind, _)| Suppressions::signal_matches(sig, kind))
            .flat_map(|(_, ls)| ls.iter().copied())
            .collect();
        lines.sort_unstable();
        lines
    }

    /// WHY an unused marker is stale — None when it is not: "stale — remove
    /// it" means exactly one case, the marker's signal fires NOWHERE in the
    /// file (a family renamed, a finding fixed). When the family still fires
    /// anywhere, the marker binds nothing but stays as documentation and the
    /// report says nothing about it (plan Phase 3).
    fn stale_reason(&self, sig: &str) -> Option<String> {
        if self.lines_for(sig).is_empty() {
            Some("nothing fires in this file — the finding was fixed, or the kind was renamed".to_string())
        } else {
            None
        }
    }
}

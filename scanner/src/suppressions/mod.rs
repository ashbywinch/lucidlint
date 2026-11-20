//! The suppression engine: `lucidlint: ignore` comments and config ignores —
//! parsing, family matching, the window a marker binds over, and the stale
//! bookkeeping that keeps a spent why from being re-flagged.
// lucidlint: ignore-file long-param-list the pass threads the scan shapes (findings,
// comments, file, marker, books) that the marker idiom reads together — grouping them
// adds a hop for the two callers

use std::collections::HashMap;

use ruff_python_ast::token::{TokenKind, Tokens};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;

mod marker;
mod stale;

use marker::LineMarkerCtx;
use stale::StaleCtx;

/// One line-level suppression and the (signal, why) parsed from it.
/// Line suppressions exempt a finding on that line or the line before;
/// file suppressions exempt the signal anywhere in the file (with a why).
/// The marker window is two anchors, each with its own line and the line above;
/// the capacity hint says so without a bare 4.
const WINDOW_CAPACITY: usize = 4;

#[derive(Clone, Default)]
pub struct Suppressions {
    pub line: HashMap<usize, Vec<(String, String)>>,
    pub file: HashMap<String, String>,
}

pub use crate::rules_gen::FAMILY_VARIANTS;

/// Suppressions the caller's own filtering paths already honored (the Rust
/// cc-array retain removes complexity findings before this filter runs) —
/// stale detection must not re-flag them.
/// Ledger handles for one suppression pass: what was already honored
/// upstream (pre_used), and where newly consumed pairs are recorded (spent).
pub struct SuppressionBooks<'a> {
    pub pre_used: &'a PreUsedSuppressions,
    pub spent: &'a mut std::collections::HashSet<(usize, String)>,
}

#[derive(Default)]
pub struct PreUsedSuppressions {
    pub lines: std::collections::HashSet<(usize, String)>,
    pub files: std::collections::HashSet<String>,
}

impl Suppressions {
    /// Parse `lucidlint: ignore <signal> <why>` / `ignore-file` comments.
    /// `comments` are (line, full comment text incl. the marker) — each language
    /// layer extracts them its own way (ruff tokens for Python, a string-aware
    /// scan for Rust); the parse and the matching are shared. A marker binds by
    /// its signal name alone: the why may truncate at line end (a repo marker
    /// cut mid-expression still binds — G5); only a marker with NO why at all
    /// is why-less and does not bind.
    pub fn from_comments(comments: &[(usize, String)]) -> Suppressions {
        let mut line_map: HashMap<usize, Vec<(String, String)>> = HashMap::new();
        let mut file_map = HashMap::new();
        for (ln, text) in comments {
            let trimmed = text.trim_start_matches(['#', '/']).trim_start();
            if let Some(rest) = trimmed.strip_prefix("lucidlint: ignore-file ") {
                let mut it = rest.splitn(2, char::is_whitespace);
                let signal = it.next().unwrap_or("").to_string();
                let why = it.next().unwrap_or("").trim().to_string();
                if !signal.is_empty() {
                    file_map.insert(signal, why);
                }
            } else if let Some(rest) = trimmed.strip_prefix("lucidlint: ignore ") {
                let mut it = rest.splitn(2, char::is_whitespace);
                let signals = it.next().unwrap_or("");
                let why = it.next().unwrap_or("").trim().to_string();
                // comma-separated signals let one comment exempt several families
                // (`ignore long-param-list,detached-method <why>`) — stacked
                // comments only fit the line/line-1 window for the last one
                for sig in signals.split(',') {
                    let sig = sig.trim();
                    if !sig.is_empty() {
                        line_map.entry(*ln).or_default().push((sig.to_string(), why.clone()));
                    }
                }
            }
        }
        Suppressions {
            line: line_map,
            file: file_map,
        }
    }

    /// Family -> variant kinds, for family suppressions (`ignore latent-class
    /// <why>` exempts every variant). GENERATED from the rule catalog
    /// (rule_metadata.py) by `make rules` — a variant missing here is a
    /// registration drift, not a judgment call (review-log B6: strewing was
    /// missing from the hand-written map and `ignore latent-class` was silently
    /// stale against it).
    fn alias_variants(sig: &str) -> &'static [&'static str] {
        for (fam, vars) in FAMILY_VARIANTS {
            if *fam == sig {
                return vars;
            }
        }
        &[]
    }

    /// Does a suppression signal match a finding's raw `kind`? Raw-equal, or the
    /// signal names the family that contains the kind.
    pub fn signal_matches(sig: &str, finding_kind: &str) -> bool {
        sig == finding_kind || Self::alias_variants(sig).contains(&finding_kind)
    }

    /// How far above a finding a suppression comment may sit, in the plain
    /// line-pair rule: the finding's own line and the line immediately before it.
    /// A marker two lines above (a decorator line intervening, say) binds nothing
    /// — and, because the signal still fires somewhere, it is documentation
    /// rather than stale. A finding whose construct spans several source lines
    /// also admits its logical line's pair (`finding_window`); this constant
    /// governs the physical pair and the complexity summaries, which are not
    /// Finding records and carry no logical line.
    const SUPPRESSION_WINDOW: usize = 2;

    /// The `Self::SUPPRESSION_WINDOW` lines ending at `line` (descending), never below 1.
    pub fn window_lines(line: usize) -> impl Iterator<Item = usize> {
        (line.max(Self::SUPPRESSION_WINDOW) + 1 - Self::SUPPRESSION_WINDOW..=line).rev()
    }

    /// The lines a marker may sit on to bind `f`, nearest first: the finding's own
    /// line and the line before it, UNION the logical line's pair — the `def` line
    /// for a signature finding, the enclosing statement's first line for a wrapped
    /// call or split literal (Phase 4a). One line of code can span several source
    /// lines, so the practiced "marker directly above the `def`" placement binds
    /// every finding anchored on the signature, however deep the finding's own
    /// source line sits.
    pub fn finding_window(f: &crate::Finding) -> Vec<usize> {
        let logical = f.logical_line();
        let mut lines: Vec<usize> = Vec::with_capacity(WINDOW_CAPACITY);
        for l in [f.line, f.line.saturating_sub(1), logical, logical.saturating_sub(1)] {
            if l >= 1 && !lines.contains(&l) {
                lines.push(l);
            }
        }
        lines.sort_unstable();
        lines.reverse();
        lines
    }

    /// A finding is exempt when an explained file suppression covers it.
    pub fn file_suppressed(signal: &str, supps: &Suppressions) -> bool {
        supps
            .file
            .iter()
            .any(|(sig, why)| Self::signal_matches(sig, signal) && !why.is_empty())
    }

    /// Repo-wide findings (duplicate, unused) are computed AFTER the per-file
    /// suppression pass, so a comment naming them was never consumed and got
    /// flagged stale (review-log B3). Re-honor their suppressions here with the
    /// same family-aware, widened window the per-file pass uses, and report the
    /// (line, signal) / file-signal pairs consumed so the caller can drop the
    /// stale-suppression findings those comments caused. Line 0 in a used pair
    /// means a FILE suppression (the `(0, sig)` sentinel).
    pub fn filter_repo_wide(
        findings: Vec<crate::Finding>,
        supps: &Suppressions,
        used_line: &mut std::collections::HashSet<(usize, String)>,
        used_file: &mut std::collections::HashSet<String>,
        spent: &std::collections::HashSet<(usize, String)>,
    ) -> Vec<crate::Finding> {
        let mut kept = Vec::new();
        for f in findings {
            if Self::file_suppressed(&f.kind, supps) {
                for (sig, why) in &supps.file {
                    if why.is_empty() {
                        continue;
                    }
                    let _ = why;
                    if Self::signal_matches(sig, &f.kind) {
                        used_file.insert(sig.clone());
                        break;
                    }
                }
                continue;
            }
            let mut line_hit = false;
            for ln in Self::finding_window(&f) {
                if let Some(entries) = supps.line.get(&ln) {
                    for (sig, why) in entries {
                        if why.is_empty() || spent.contains(&(ln, sig.clone())) {
                            continue;
                        }
                        if Self::signal_matches(sig, &f.kind) {
                            used_line.insert((ln, sig.clone()));
                            line_hit = true;
                            break;
                        }
                    }
                }
                if line_hit {
                    break;
                }
            }
            if line_hit {
                continue;
            }
            kept.push(f);
        }
        kept
    }

    /// The Python `_suppressed`: a finding is exempt when any of the lines directly
    /// above it carry an explained suppression for that signal.
    pub fn suppressed(signal: &str, line: usize, supps: &Suppressions) -> bool {
        for ln in Self::window_lines(line) {
            if let Some(entries) = supps.line.get(&ln) {
                for (sig, why) in entries {
                    if Self::signal_matches(sig, signal) && !why.is_empty() {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Partition finding indices into (file, line) groups — inner-first within
    /// each line (higher col = deeper literal), stable otherwise.
    fn common_group_line_indices(findings: &[crate::Finding]) -> Vec<Vec<usize>> {
        let mut order: Vec<usize> = (0..findings.len()).collect();
        order.sort_by(|&a, &b| {
            findings[a]
                .file
                .cmp(&findings[b].file)
                .then(findings[a].line.cmp(&findings[b].line))
                .then(findings[b].col.cmp(&findings[a].col))
                .then(a.cmp(&b))
        });
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for i in order {
            match groups.last_mut() {
                Some(g) if findings[g[0]].file == findings[i].file && findings[g[0]].line == findings[i].line => {
                    g.push(i);
                }
                _ => groups.push(vec![i]),
            }
        }
        groups
    }

    /// Marker inventory for one line-group: every explained marker line whose
    /// signal matches any member, one pair per line. A pair another line-group
    /// consumed is NOT filtered here — one marker covers every member of the
    /// site it sits on (own line or the line before), recorded spent once (set
    /// semantics). The window is the union of the members' two-line-plus-logical
    /// windows: a def's signature findings share the def line even when their own
    /// lines differ.
    fn marker_inventory(members: &[usize], findings: &[crate::Finding], supps: &Suppressions) -> Vec<(usize, String)> {
        let mut window: Vec<usize> = Vec::new();
        for &i in members {
            for ln in Self::finding_window(&findings[i]) {
                if !window.contains(&ln) {
                    window.push(ln);
                }
            }
        }
        window.sort_unstable();
        window.reverse();
        let mut pairs: Vec<(usize, String)> = Vec::new();
        for ln in window {
            if let Some(entries) = supps.line.get(&ln) {
                for (sig, why) in entries {
                    if why.is_empty() {
                        continue;
                    }
                    // one marker LINE may name several signals
                    // (`ignore closures,long-param-list <why>`); each pair is its
                    // own candidate, so every named signal can bind its finding
                    if pairs.iter().any(|(pl, ps)| *pl == ln && ps == sig) {
                        continue;
                    }
                    if members.iter().any(|&i| Self::signal_matches(sig, &findings[i].kind)) {
                        pairs.push((ln, sig.clone()));
                    }
                }
            }
        }
        pairs
    }

    /// Per-signal line index — the facts behind the stale binding reasons
    /// (finding gone / covered by another marker / outside the window).
    fn signal_line_index(findings: &[crate::Finding]) -> std::collections::HashMap<String, Vec<usize>> {
        let mut by_signal: std::collections::HashMap<String, Vec<usize>> = Default::default();
        for f in findings {
            by_signal.entry(f.kind.clone()).or_default().push(f.line);
        }
        by_signal
    }

    /// Filter findings through the suppressions + emit the why-less suppression
    /// findings — the shared post-filter (Python `_scan_file`'s). `marker` is the
    /// comment token ('#' or "//") used in the why-less messages. `pre_used`
    /// carries the suppressions the caller's cc-array retain already honored so
    /// stale detection does not re-flag them (the Rust layer's cc path).
    /// `magic_exempt_labels` names the exemption kinds among the file's
    /// still-present magic-number candidates — the stale magic-number message
    /// then appends the exact delta (G4) instead of reading as if the numbers
    /// were gone or claiming exemptions that do not apply.
    pub fn apply(
        findings: Vec<crate::Finding>,
        comments: &[(usize, String)],
        file: &str,
        marker: &str,
        books: &mut SuppressionBooks,
        magic_exempt_labels: &[&'static str],
    ) -> Vec<crate::Finding> {
        let supps = Suppressions::from_comments(comments);
        let mut out = Vec::new();
        let mut used_line: std::collections::HashSet<(usize, String)> = books.pre_used.lines.clone();
        let mut used_file: std::collections::HashSet<String> = books.pre_used.files.clone();
        let mut seen_invalid: std::collections::HashSet<(usize, String)> = std::collections::HashSet::new();
        for (ln, entries) in &supps.line {
            for (sig, why) in entries {
                if why.is_empty() && seen_invalid.insert((*ln, sig.clone())) {
                    out.push(crate::Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                file: file.to_string(),
                                line: *ln,
                                function: String::new(),
                                kind: "suppression".into(),
                                severity: "fail".into(),
                                message: format!(
                                    "suppression '{marker} lucidlint: ignore {sig}' at line {ln} without a why — exemptions only apply with an explanation"
                                ), });
                }
            }
        }
        for (sig, why) in &supps.file {
            if why.is_empty() {
                if let Some((ln, _)) = comments
                    .iter()
                    .find(|(_, t)| t.contains(&format!("lucidlint: ignore-file {sig}")))
                {
                    out.push(crate::Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                    file: file.to_string(),
                                    line: *ln,
                                    function: String::new(),
                                    kind: "suppression".into(),
                                    severity: "fail".into(),
                                    message: format!(
                                        "file suppression '{marker} lucidlint: ignore-file {sig}' at line {ln} without a why — exemptions only apply with an explanation"
                                    ), });
                }
            }
        }
        let mut exempted = vec![false; findings.len()];
        let mut taken: std::collections::HashSet<(usize, String)> = Default::default();
        // innermost-peel: markers bind inner-first, one marker per finding
        let by_signal = Self::signal_line_index(&findings);
        for members in Self::common_group_line_indices(&findings) {
            let mut ctx = LineMarkerCtx {
                supps: &supps,
                used_line: &mut used_line,
                taken: &mut taken,
                spent: books.spent,
            };
            let ok = ctx.peel(&members, &findings);
            for (j, &i) in members.iter().enumerate() {
                if ok[j] {
                    exempted[i] = true;
                    continue;
                }
                if Self::suppress_track_file(&mut used_file, &supps, &findings[i]) {
                    exempted[i] = true;
                }
            }
        }
        out.extend(
            findings
                .into_iter()
                .enumerate()
                .filter(|(i, _)| !exempted[*i])
                .map(|(_, f)| f),
        );
        let ctx = StaleCtx {
            supps: &supps,
            used_line: &used_line,
            used_file: &used_file,
            comments,
            file,
            marker,
            by_signal: &by_signal,
            magic_exempt_labels,
        };
        out.extend(ctx.stale_suppression_findings());
        out
    }

    /// Was this finding exempted by an explained file suppression?
    fn suppress_track_file(
        used_file: &mut std::collections::HashSet<String>,
        supps: &Suppressions,
        f: &crate::Finding,
    ) -> bool {
        if let Some(why) = supps.file.get(&f.kind) {
            if !why.is_empty() {
                used_file.insert(f.kind.clone());
                return true;
            }
        }
        // the file suppression may name a FAMILY (latent-class) — then it covers
        // the variant raw kinds (closures/partition), not just one exact kind
        for (sig, why) in &supps.file {
            if Self::matches_suppression(sig, f, why) {
                used_file.insert(sig.clone());
                return true;
            }
        }
        false
    }

    /// The suppression entry names this finding's family, with a reason.
    pub fn matches_suppression(sig: &str, finding: &crate::Finding, why: &str) -> bool {
        sig != finding.kind && Self::signal_matches(sig, &finding.kind) && !why.is_empty()
    }

    /// Comment tokens -> (line, comment text incl. '#') — from the parsed
    /// token stream (0.0.9's lexer exposes no public range accessor).
    pub fn comment_lines(source: &str, tokens: &Tokens) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        // tokens are in source order — count newlines between comment starts
        // instead of re-scanning from offset 0 per comment (`line_of` is O(n);
        // N comments used to cost O(N^2) — a comment-heavy file scanned 4x per
        // run and went quadratic)
        let mut line = 1usize;
        let mut prev = 0usize;
        for tok in tokens.iter() {
            if tok.kind() == TokenKind::Comment {
                let range = tok.range();
                let start = range.start().to_usize();
                line += source[prev..start].bytes().filter(|&b| b == b'\n').count();
                prev = start;
                let text = &source[range];
                out.push((line, text.to_string()));
            }
        }
        out
    }

    /// Python comment tokens -> the shared suppression map (`#` comments).
    pub fn parse_suppressions(source: &str, tokens: &Tokens) -> crate::suppressions::Suppressions {
        Suppressions::from_comments(&Self::comment_lines(source, tokens))
    }

    /// Map every 1-based source line to the first line of its LOGICAL line.
    ///
    /// One line of code can span several source lines — a multi-line `def`, a
    /// wrapped call, a split literal. Python joins those physical lines into one
    /// logical line (implicit joining inside brackets, explicit `\` joining); the
    /// token stream marks the logical breaks with `Newline` and the joins with
    /// `NonLogicalNewline`, so the first significant token after each logical
    /// break is the logical line's first source line. Comments and blank lines
    /// are not part of any logical line: they map to themselves, and a finding
    /// never anchors on them.
    pub fn logical_line_starts(source: &str, tokens: &Tokens) -> Vec<usize> {
        let nlines = source.lines().count();
        // identity by default: a line is its own logical start
        let mut starts: Vec<usize> = (0..=nlines).collect();
        let mut cur: Option<usize> = None;
        let mut filled = 0usize;
        for tok in tokens.iter() {
            let line = Source::line_of(source, tok.range().start()).min(nlines);
            if line > filled {
                match tok.kind() {
                    TokenKind::Newline => {
                        // the logical line ends here — every line from the last
                        // break to this one belongs to `cur`
                        let val = cur.take();
                        for slot in starts.iter_mut().take(line + 1).skip(filled + 1) {
                            let l = filled + 1;
                            *slot = match val {
                                Some(v) if v <= l => v,
                                _ => l,
                            };
                            filled = l;
                        }
                        filled = line;
                    }
                    // markers, joins, block structure: any of these can be the
                    // only token on its line without starting a logical line
                    TokenKind::Comment
                    | TokenKind::NonLogicalNewline
                    | TokenKind::Indent
                    | TokenKind::Dedent
                    | TokenKind::EndOfFile => {}
                    _ => {
                        if cur.is_none() {
                            cur = Some(line);
                        }
                    }
                }
            }
        }
        let val = cur;
        for (offset, slot) in starts.iter_mut().enumerate().take(nlines + 1).skip(filled + 1) {
            *slot = match val {
                Some(v) if v <= offset => v,
                _ => offset,
            };
        }
        starts
    }

    /// Filter findings through the suppressions + emit the why-less suppression
    /// findings — shared logic over this language's comment lines (the parse
    /// itself lives in `crate::suppressions::suppressions_from_comments`). `pre_used` carries
    /// the suppressions the cc-retain already honored (complexity suppressions
    /// are applied before the findings filter; stale detection must not re-flag
    /// them). `magic_exempt_labels` names the exemption kinds among the file's
    /// still-present magic-number candidates — the stale message cites exactly
    /// those (G4), never a blanket unit/constant/table claim.
    pub fn apply_source(
        findings: Vec<Finding>,
        source: &str,
        file: &str,
        tokens: &Tokens,
        books: &mut crate::suppressions::SuppressionBooks,
        magic_exempt_labels: &[&'static str],
    ) -> Vec<Finding> {
        Suppressions::apply(
            findings,
            &Self::comment_lines(source, tokens),
            file,
            "#",
            books,
            magic_exempt_labels,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::fix_render::FixRender;

    #[test]
    fn complexity_message_routes_by_shape() {
        // review: a dispatch chain's complexity suggestion must name the
        // registry, a rule battery the named checks — not a blind
        // extract-method (the directive stays extract-method: it is the real
        // auto-fix; the prose names the more lucid shape)
        let dispatch = FixRender::complexity_message(36, "dispatch", "tool");
        assert!(dispatch.contains("dispatch chain over 'tool'"), "{dispatch}");
        assert!(dispatch.contains("HOIST THE HIDDEN DATA STRUCTURE"), "{dispatch}");
        assert!(dispatch.contains("lambda closures"), "{dispatch}");
        assert!(dispatch.contains("fix: dispatch-registry"), "{dispatch}");
        let rules = FixRender::complexity_message(42, "rules", "violations");
        assert!(rules.contains("HOIST THE HIDDEN DATA STRUCTURE"), "{rules}");
        assert!(rules.contains("(condition, violation) table"), "{rules}");
        assert!(rules.contains("fix: rule-table"), "{rules}");
        let plain = FixRender::complexity_message(20, "plain", "");
        assert!(
            plain.contains("extract part of this function into a named method"),
            "{plain}"
        );
    }
    use crate::Finding;

    fn finding(kind: &str, line: usize) -> Finding {
        Finding {
            logical_start: None,
            seam_members: Vec::new(),
            file: "x.rs".into(),
            line,
            col: 0,
            function: "f".into(),
            kind: kind.into(),
            severity: "fail".into(),
            message: "m".into(),
        }
    }

    #[test]
    fn a_marker_names_only_its_own_signal_at_a_shared_anchor() {
        // a `closures` marker above a def used to exempt EVERY finding at that
        // col — including a large-function reported on the same anchor, so a
        // 372-line function silently vanished from the gate. The marker binds
        // the members its signal names and leaves the rest.
        let comments = vec![(1, "// lucidlint: ignore closures the walker is one pass".to_string())];
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let fs = Suppressions::apply(
            vec![finding("closures", 2), finding("large-function", 2)],
            &comments,
            "x.rs",
            "//",
            &mut books,
            &[],
        );
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].kind, "large-function");
    }

    #[test]
    fn a_comma_marker_covers_each_named_signal_at_one_anchor() {
        // the sanctioned way to exempt two findings on one def: one marker
        // naming both signals (`ignore closures,large-function <why>`) — the
        // why sits in the ONE line that fits the window
        let comments = vec![(
            1,
            "// lucidlint: ignore closures,large-function one site, both shapes".to_string(),
        )];
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let fs = Suppressions::apply(
            vec![finding("closures", 2), finding("large-function", 2)],
            &comments,
            "x.rs",
            "//",
            &mut books,
            &[],
        );
        assert!(fs.is_empty(), "{:?}", fs.iter().map(|f| &f.kind).collect::<Vec<_>>());
    }

    #[test]
    fn family_suppression_matches_variant_kind_not_stale() {
        // B6: `ignore latent-class` suppresses a `closures` finding — the
        // suppression names the FAMILY, the finding carries the RAW kind —
        // and must not be reported stale.
        let comments = vec![(
            1,
            "// lucidlint: ignore latent-class route closures are the idiom".to_string(),
        )];
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let fs = Suppressions::apply(vec![finding("closures", 2)], &comments, "x.rs", "//", &mut books, &[]);
        assert!(!fs.iter().any(|f| f.kind == "closures"), "{:?}", fs);
        assert!(!fs.iter().any(|f| f.kind == "stale-suppression"), "{:?}", fs);
    }

    #[test]
    fn family_suppression_against_no_variant_is_stale() {
        // B6 control: the same family suppression with no closures/partition
        // finding is dead weight → stale-suppression.
        let comments = vec![(1, "// lucidlint: ignore latent-class nothing here".to_string())];
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let fs = Suppressions::apply(vec![], &comments, "x.rs", "//", &mut books, &[]);
        assert!(fs.iter().any(|f| f.kind == "stale-suppression"), "{:?}", fs);
    }
    #[test]
    fn stale_message_directive_only_where_a_fixer_exists() {
        // The `— fix: stale-suppression` directive must appear only where
        // the fix command can actually Self::apply (the Python engine deletes the
        // marker; the Rust core has no such fixer) — an advertised fix that
        // cannot run is a false suggestion (user ruling).
        let comments = vec![(1, "// lucidlint: ignore latent-class nothing here".to_string())];
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let rs = Suppressions::apply(vec![], &comments, "x.rs", "//", &mut books, &[]);
        let msg = rs
            .iter()
            .find(|f| f.kind == "stale-suppression")
            .unwrap()
            .message
            .clone();
        assert!(!msg.contains("fix:"), "{msg}");
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let py = Suppressions::apply(vec![], &comments, "x.py", "#", &mut books, &[]);
        let msg = py
            .iter()
            .find(|f| f.kind == "stale-suppression")
            .unwrap()
            .message
            .clone();
        assert!(msg.contains("fix: stale-suppression"), "{msg}");
    }

    #[test]
    fn stale_only_when_signal_fires_nowhere() {
        // Phase 3: "stale — remove it" is reserved for the ONE case it
        // means — the marker's signal fires NOWHERE in the file. When the
        // family still fires anywhere, the marker is documentation: no
        // stale-suppression finding at all, never a mis-placed verdict,
        // never advice to move the marker (the rightmove_url:57 shape).
        let mut spent = std::collections::HashSet::new();
        let gone = Suppressions::apply(
            vec![],
            &[(1, "// lucidlint: ignore magic-number gone".to_string())],
            "x.py",
            "#",
            &mut SuppressionBooks {
                pre_used: &PreUsedSuppressions::default(),
                spent: &mut spent,
            },
            &["unit-named", "constant-definition", "data-table"],
        );
        let msg = gone
            .iter()
            .find(|f| f.kind == "stale-suppression")
            .unwrap()
            .message
            .clone();
        assert!(msg.contains("nothing fires in this file"), "{msg}");
        // G4: the rule delta — the literals are STILL in the file, so the
        // message names EXACTLY the exemption kinds passed (unit-named and
        // constant-definition and data-table), not a blanket claim
        assert!(
            msg.contains("the file still holds numeric literals — now covered by the unit-named, constant-definition, and data-table exemptions; verify the reason before deleting the marker"),
            "{msg}"
        );

        // control: the same marker over a file with NO numeric literals
        // keeps the plain fires-nowhere text — no delta clause
        let mut spent = std::collections::HashSet::new();
        let bare = Suppressions::apply(
            vec![],
            &[(1, "// lucidlint: ignore magic-number gone".to_string())],
            "x.py",
            "#",
            &mut SuppressionBooks {
                pre_used: &PreUsedSuppressions::default(),
                spent: &mut spent,
            },
            &[],
        );
        let msg = bare
            .iter()
            .find(|f| f.kind == "stale-suppression")
            .unwrap()
            .message
            .clone();
        assert!(msg.contains("nothing fires in this file"), "{msg}");
        assert!(!msg.contains("the file still holds numeric literals"), "{msg}");

        // marker below the finding: the window of line 3 is 1..=3, so a
        // marker at 5 cannot bind — the family still fires, so the marker
        // stays as documentation and NO stale verdict is emitted
        let mut spent = std::collections::HashSet::new();
        let misplaced = Suppressions::apply(
            vec![finding("magic-number", 3)],
            &[(5, "// lucidlint: ignore magic-number threshold".to_string())],
            "x.py",
            "#",
            &mut SuppressionBooks {
                pre_used: &PreUsedSuppressions::default(),
                spent: &mut spent,
            },
            &[],
        );
        assert!(
            !misplaced.iter().any(|f| f.kind == "stale-suppression"),
            "{misplaced:?}"
        );

        // two markers, one finding: the loser's window is fully covered —
        // the signal still fires, so the loser is documentation too
        let mut spent = std::collections::HashSet::new();
        let covered = Suppressions::apply(
            vec![finding("magic-number", 3)],
            &[
                (2, "// lucidlint: ignore magic-number first".to_string()),
                (3, "// lucidlint: ignore magic-number second".to_string()),
            ],
            "x.py",
            "#",
            &mut SuppressionBooks {
                pre_used: &PreUsedSuppressions::default(),
                spent: &mut spent,
            },
            &[],
        );
        assert!(!covered.iter().any(|f| f.kind == "stale-suppression"), "{covered:?}");
    }

    #[test]
    fn stale_marker_one_line_past_window_stays_documentation() {
        // The fires-nowhere distinction, per signal: a marker past the
        // two-line window over a family firing below (marker at 3, window of
        // line 6 is 5..=6) gets no verdict and no advice; the SAME marker
        // would be stale if the family fired nowhere at all.
        let mut spent = std::collections::HashSet::new();
        let near = Suppressions::apply(
            vec![finding("magic-number", 6), finding("magic-number", 9)],
            &[(3, "// lucidlint: ignore magic-number threshold".to_string())],
            "x.py",
            "#",
            &mut SuppressionBooks {
                pre_used: &PreUsedSuppressions::default(),
                spent: &mut spent,
            },
            &[],
        );
        assert!(!near.iter().any(|f| f.kind == "stale-suppression"), "{near:?}");
        assert!(near.iter().any(|f| f.kind == "magic-number"), "{near:?}");

        let mut spent = std::collections::HashSet::new();
        let far = Suppressions::apply(
            vec![],
            &[(3, "// lucidlint: ignore magic-number threshold".to_string())],
            "x.py",
            "#",
            &mut SuppressionBooks {
                pre_used: &PreUsedSuppressions::default(),
                spent: &mut spent,
            },
            &[],
        );
        let msg = far
            .iter()
            .find(|f| f.kind == "stale-suppression")
            .unwrap()
            .message
            .clone();
        assert!(msg.contains("nothing fires in this file"), "{msg}");
        assert!(!msg.contains("nearest matching finding"), "{msg}");
    }

    #[test]
    fn stale_delta_names_only_the_applicable_exemption_kinds() {
        // G4: the appended exemption clause lists EXACTLY the kinds that
        // cover the file's remaining literals — a position/index-only file
        // never claims unit/constant/table, and no candidates means no
        // clause at all.
        let mut spent = std::collections::HashSet::new();
        let index_only = Suppressions::apply(
            vec![],
            &[(1, "// lucidlint: ignore magic-number gone".to_string())],
            "x.py",
            "#",
            &mut SuppressionBooks {
                pre_used: &PreUsedSuppressions::default(),
                spent: &mut spent,
            },
            &["position/index"],
        );
        let msg = index_only
            .iter()
            .find(|f| f.kind == "stale-suppression")
            .unwrap()
            .message
            .clone();
        assert!(msg.contains("now covered by the position/index exemption"), "{msg}");
        assert!(!msg.contains("unit-named"), "{msg}");
        assert!(!msg.contains("constant-definition"), "{msg}");
        assert!(!msg.contains("data-table"), "{msg}");

        // no candidates -> the marker reads as a plain fix/rename stale,
        // with no exemption clause
        let mut spent = std::collections::HashSet::new();
        let none = Suppressions::apply(
            vec![],
            &[(1, "// lucidlint: ignore magic-number gone".to_string())],
            "x.py",
            "#",
            &mut SuppressionBooks {
                pre_used: &PreUsedSuppressions::default(),
                spent: &mut spent,
            },
            &[],
        );
        let msg = none
            .iter()
            .find(|f| f.kind == "stale-suppression")
            .unwrap()
            .message
            .clone();
        assert!(msg.contains("nothing fires in this file"), "{msg}");
        assert!(!msg.contains("covered by the"), "{msg}");
    }

    #[test]
    fn file_family_suppression_matches_variant() {
        // B6 file path: `ignore-file latent-class` covers closures/partition.
        let comments = vec![(
            1,
            "// lucidlint: ignore-file latent-class the whole file's closures are idiom".to_string(),
        )];
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let fs = Suppressions::apply(vec![finding("partition", 9)], &comments, "x.rs", "//", &mut books, &[]);
        assert!(!fs.iter().any(|f| f.kind == "partition"), "{:?}", fs);
        assert!(!fs.iter().any(|f| f.kind == "stale-suppression"), "{:?}", fs);
    }
    #[test]
    fn family_suppression_matches_strewing_variant() {
        // B6: the third latent-class variant. `ignore latent-class` must
        // suppress a `strewing` finding too — final_kind collapses it into
        // the family, so the family keyword without the variant is the exact
        // stale-suppression trap the review log hit.
        let comments = vec![(
            1,
            "// lucidlint: ignore latent-class the helpers are the module's seams".to_string(),
        )];
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let fs = Suppressions::apply(vec![finding("strewing", 2)], &comments, "x.py", "#", &mut books, &[]);
        assert!(!fs.iter().any(|f| f.kind == "strewing"), "{:?}", fs);
        assert!(!fs.iter().any(|f| f.kind == "stale-suppression"), "{:?}", fs);
    }

    #[test]
    fn peel_searches_inventory_not_pointer() {
        // group members sort by col DESC (inner-first) while the marker
        // inventory sorts by line DESC — the deeper finding's marker on an
        // EARLIER window line strands under pointer-only assignment: the
        // shallower member consumes its marker, the deep member's valid
        // suppression is silently ignored (review bot)
        let comments = vec![
            (1, "# lucidlint: ignore magic-number why-b".to_string()),
            (2, "# lucidlint: ignore middle-man why-a".to_string()),
        ];
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let deep_magic = Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col: 5,
            ..finding("magic-number", 2)
        };
        let fs = Suppressions::apply(
            vec![deep_magic, finding("middle-man", 2)],
            &comments,
            "x.py",
            "#",
            &mut books,
            &[],
        );
        assert!(
            !fs.iter().any(|f| f.kind == "magic-number" || f.kind == "middle-man"),
            "both markers must bind regardless of order skew: {:?}",
            fs
        );
    }

    #[test]
    fn marker_two_lines_above_does_not_bind_and_is_not_stale() {
        // Phase 4: a comment two lines above the finding (a decorator line
        // intervenes) binds nothing — the window is the finding's own line and
        // the line before (its logical line, when it has one, adds its own
        // pair — these test findings carry none). The family still fires, so
        // the marker is documentation, never stale.
        let comments = vec![(1, "// lucidlint: ignore magic-number the gate threshold".to_string())];
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let fs = Suppressions::apply(
            vec![finding("magic-number", 3)],
            &comments,
            "x.rs",
            "//",
            &mut books,
            &[],
        );
        assert!(fs.iter().any(|f| f.kind == "magic-number"), "{:?}", fs);
        assert!(!fs.iter().any(|f| f.kind == "stale-suppression"), "{:?}", fs);
    }

    #[test]
    fn window_is_bounded_far_comment_does_not_suppress() {
        // Phase 4 guard: the window stays adjacent — a comment 3+ lines above
        // is NOT a suppression of the finding (and the finding carries no
        // logical line to widen the window).
        let comments = vec![(1, "// lucidlint: ignore magic-number far away".to_string())];
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let fs = Suppressions::apply(
            vec![finding("magic-number", 5)],
            &comments,
            "x.rs",
            "//",
            &mut books,
            &[],
        );
        assert!(fs.iter().any(|f| f.kind == "magic-number"), "{:?}", fs);
    }

    #[test]
    fn logical_window_binds_on_the_logical_line_and_stays_bounded() {
        // Phase 4a: a finding on line 5 whose construct starts on line 2 (a
        // multi-line signature arm) is bound by a marker on the logical line
        // or the line before — the marker at 2 sits two lines above the
        // finding's own line and a plain two-line window would miss it.
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let sig = Finding {
            logical_start: Some(2),
            ..finding("record-shape", 5)
        };
        let bound = Suppressions::apply(
            vec![sig],
            &[(2, "// lucidlint: ignore record-shape the wire payload".to_string())],
            "x.rs",
            "//",
            &mut books,
            &[],
        );
        assert!(!bound.iter().any(|f| f.kind == "record-shape"), "{bound:?}");
        assert!(!bound.iter().any(|f| f.kind == "stale-suppression"), "{bound:?}");

        // two lines above the logical line binds nothing — and because the
        // family still fires the marker is documentation, never stale
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let far = Finding {
            logical_start: Some(3),
            ..finding("record-shape", 5)
        };
        let left = Suppressions::apply(
            vec![far],
            &[(1, "// lucidlint: ignore record-shape too far".to_string())],
            "x.rs",
            "//",
            &mut books,
            &[],
        );
        assert!(left.iter().any(|f| f.kind == "record-shape"), "{left:?}");
        assert!(!left.iter().any(|f| f.kind == "stale-suppression"), "{left:?}");
    }
}

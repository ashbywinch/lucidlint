//! The language-neutral core of the scan: logic that is IDENTICAL for every
//! language layer, parameterized only by what the language layers extract.
//!
//! The seam: a language layer (Python = `checks.rs` + the visitor in `main.rs`,
//! Rust = `rustscan.rs`) turns its own AST into the small plain shapes here
//! (comment lines, skeletons, numeric literal texts), and THIS module does the
//! reasoning — similarity, dedupe bucketing, suppression matching, the shared
//! rule tables. A check that works in both languages lives here as a pure
//! function over one of those shapes; a check that is genuinely language-
//! specific lives in the language layer and is never forced through a shared
//! abstraction (the except/broad-except family has no Rust analog, `static`
//! globals no Python analog — those stay apart by design).
//!
//! Cyclomatic-complexity rule table (radon-equivalent; authoritative here,
//! both visitors follow it):
//!   if (+1 per elif/else-if; trailing else does NOT count)  for/while/loop +1
//!   try: handlers +1 each, else +1 (Python only — Rust has no try/else)
//!   assert/assert!/debug_assert! +1, subtree NOT counted (visit_Assert)
//!   match/case: each arm minus the `_` wildcard +1
//!   bool op (and/or, &&/||): operands-1           ternary/if-expr +1
//!   comprehension generators + ifs (Python)       closures/lambdas +0, body walked
//!   nested functions: not counted in the outer function (decisions tracked per fn)
//!   base +1 for the function itself.

use std::collections::HashMap;

/// The vague role-suffix names hiding load-bearing code — one table for both
/// languages; the case differs (Python `Manager`, Rust `Manager` — same
/// UpperCamel role nouns appear in both).
pub const VAGUE_SUFFIXES: [&str; 8] = [
    "Manager",
    "Orchestrator",
    "Handler",
    "Store",
    "Repository",
    "Controller",
    "Utils",
    "Info",
];

/// A role-suffixed name is a vague-name finding only when it carries real
/// weight (large span or several methods); a thin class is the name itself
/// communicating. Pure — each language layer feeds its own (span, methods).
pub fn vague_role_is_loaded(name: &str, span_lines: usize, methods: usize) -> bool {
    VAGUE_SUFFIXES.iter().any(|s| name.ends_with(s)) && (span_lines >= 120 || methods >= 6)
}

/// Magic numbers: a numeric literal is magic when its text is outside the
/// (0, 1, 2) trivial set. The position rule (operand of an operation, not a
/// keyword value / definition site) is language-layer logic.
pub fn is_magic_value(text: &str) -> bool {
    !matches!(text, "0" | "1" | "2")
}

// --------------------------------------------------------------------------- duplicates

/// One duplicate candidate: the file/name/line plus its structural skeleton.
/// The skeleton is a language-layer product (ruff op tokens for Python, syn
/// tokens for Rust); the similarity machinery below is language-neutral.
#[derive(Clone)]
pub struct SkeletonFn {
    pub rel: String,
    pub name: String,
    pub line: usize,
    pub skeleton: Vec<String>,
}

/// Rewrite a finding message's `fix:` directive into the FULL runnable
/// command. The scanner messages say `— fix: <kind> [--fix-name <N>]`; that
/// reads as if a command named `<kind>` existed. The real surface is the
/// `fix` subcommand: `lucidlint fix --kind <kind> --file <file> --line
/// <line>` (the R27 contract: the tool owns its coordinates, so the
/// directive is self-contained and the caller never types a `.py` or a
/// `fix-` prefix). The preview-only families carry no --name — running the
/// bare command previews; adding the name applies.
pub fn full_fix_command(file: &str, line: usize, message: &str) -> String {
    let Some(pos) = message.rfind("— fix: ") else {
        return message.to_string();
    };
    let (head, tail) = message.split_at(pos);
    let dir = tail.trim_start_matches("— fix: ");
    let mut parts = dir.split_whitespace();
    let Some(fix_kind) = parts.next() else {
        return message.to_string();
    };
    let rest: Vec<&str> = parts.collect();
    let mut name_slot = String::new();
    // an EXACT --fix-name token — a prose token that merely STARTS with
    // "--fix-name" (a parenthetical like "--fix-name;") would otherwise be
    // mistaken for the machine slot and fabricate "--name <prose>" (R28:
    // the directive must be the exact command)
    if let Some(i) = rest.iter().position(|p| *p == "--fix-name") {
        if let Some(slot) = rest.get(i + 1) {
            name_slot = format!(" --name {slot}");
        }
    }
    // extract-module's member list travels as --params (the seam: which
    // module-scope defs move) — the rewrite must carry it or the agent gets
    // a command the fix cannot act on
    let mut params_slot = String::new();
    if let Some(i) = rest.iter().position(|p| *p == "--params") {
        if let Some(slot) = rest.get(i + 1) {
            params_slot = format!(" --params {slot}");
        }
    }
    format!("{head}— fix: lucidlint fix --kind {fix_kind} --file {file} --line {line}{name_slot}{params_slot}")
}

/// The complexity finding message, routed by the function's SHAPE: a
/// dispatch chain or rule battery gets the lucid refactoring for ITS shape
/// (a handler registry / named checkers), anything else gets extract-method.
/// The `fix:` directive stays extract-method — it is the real auto-fix that
/// splits the CC — while the prose names the more lucid shape (review-log
/// R1: "is extract-method the most lucid refactoring?").
pub fn complexity_message(cc: u32, shape: &str, detail: &str) -> String {
    // each shape carries ITS OWN fix directive — the shared extract-method
    // tail would append a second, wrong directive to the shape-routed ones
    match shape {
        "dispatch" => format!(
            "cyclomatic complexity {cc} (>= 15) — the function is a dispatch chain over '{detail}': every arm is a named handler — HOIST THE LATENT DATA STRUCTURE: the chain IS a (selector → action) table — collapse it into a dict of {detail} → lambda closures in Python (a match in Rust), and dispatch by lookup — fix: dispatch-registry (previews the table; apply with --confirm)"
        ),
        "rules" => format!(
            "cyclomatic complexity {cc} (>= 15) — the function is a battery of independent checks each appending to '{detail}' — HOIST THE LATENT DATA STRUCTURE: the if/append chain IS a (condition, violation) table — collapse it into a list of such pairs whose conditions are lambdas (Python) or fn pointers (Rust), and collect the violations whose condition holds — fix: rule-table (previews the table; apply with --confirm)"
        ),
        _ => format!(
            "cyclomatic complexity {cc} (>= 15) — extract part of this function into a named method (the preview shows the block) — fix: extract-method"
        ),
    }
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

/// One line-level suppression and the (signal, why) parsed from it.
/// Line suppressions exempt a finding on that line or the line before;
/// file suppressions exempt the signal anywhere in the file (with a why).
#[derive(Clone, Default)]
pub struct Suppressions {
    pub line: HashMap<usize, Vec<(String, String)>>,
    pub file: HashMap<String, String>,
}

/// Parse `lucidlint: ignore <signal> <why>` / `ignore-file` comments.
/// `comments` are (line, full comment text incl. the marker) — each language
/// layer extracts them its own way (ruff tokens for Python, a string-aware
/// scan for Rust); the parse and the matching are shared.
pub fn suppressions_from_comments(comments: &[(usize, String)]) -> Suppressions {
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
pub use crate::rules_gen::FAMILY_VARIANTS;

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
    sig == finding_kind || alias_variants(sig).contains(&finding_kind)
}

/// How far above a finding a suppression comment may sit. A suppression sits
/// "directly above" its code, but a decorator line (`@final`) or a stacked
/// comment/blank line intervenes — a fixed line/line-1 window breaks that
/// (RUST-CORE B7). A 3-line window clears one intervening line while staying
/// "adjacent" — far enough that a deliberate comment is never orphaned, close
/// enough that it cannot drift onto an unrelated statement.
const SUPPRESSION_WINDOW: usize = 3;

/// The `SUPPRESSION_WINDOW` lines ending at `line` (descending), never below 1.
pub fn window_lines(line: usize) -> impl Iterator<Item = usize> {
    (line.max(SUPPRESSION_WINDOW) + 1 - SUPPRESSION_WINDOW..=line).rev()
}

/// A finding is exempt when an explained file suppression covers it.
pub fn file_suppressed(signal: &str, supps: &Suppressions) -> bool {
    supps
        .file
        .iter()
        .any(|(sig, why)| signal_matches(sig, signal) && !why.is_empty())
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
        if file_suppressed(&f.kind, supps) {
            for (sig, why) in &supps.file {
                if why.is_empty() {
                    continue;
                }
                let _ = why;
                if signal_matches(sig, &f.kind) {
                    used_file.insert(sig.clone());
                    break;
                }
            }
            continue;
        }
        let mut line_hit = false;
        for ln in window_lines(f.line) {
            if let Some(entries) = supps.line.get(&ln) {
                for (sig, why) in entries {
                    if why.is_empty() || spent.contains(&(ln, sig.clone())) {
                        continue;
                    }
                    if signal_matches(sig, &f.kind) {
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
    for ln in window_lines(line) {
        if let Some(entries) = supps.line.get(&ln) {
            for (sig, why) in entries {
                if signal_matches(sig, signal) && !why.is_empty() {
                    return true;
                }
            }
        }
    }
    false
}

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

/// Marker inventory for one line-group: distinct explained marker lines whose
/// signal matches any member, minus globally spent/taken pairs.
fn marker_inventory(
    members: &[usize],
    findings: &[crate::Finding],
    supps: &Suppressions,
    used_line: &std::collections::HashSet<(usize, String)>,
    taken: &std::collections::HashSet<(usize, String)>,
) -> Vec<(usize, String)> {
    let mut pairs: Vec<(usize, String)> = Vec::new();
    for ln in window_lines(findings[members[0]].line) {
        if let Some(entries) = supps.line.get(&ln) {
            for (sig, why) in entries {
                if why.is_empty()
                    || used_line.contains(&(ln, sig.clone()))
                    || taken.contains(&(ln, sig.clone()))
                    || pairs.iter().any(|(pl, _)| *pl == ln)
                {
                    continue;
                }
                if members.iter().any(|&i| signal_matches(sig, &findings[i].kind)) {
                    pairs.push((ln, sig.clone()));
                }
            }
        }
    }
    pairs
}

struct LineMarkerCtx<'a> {
    supps: &'a Suppressions,
    used_line: &'a mut std::collections::HashSet<(usize, String)>,
    taken: &'a mut std::collections::HashSet<(usize, String)>,
    spent: &'a mut std::collections::HashSet<(usize, String)>,
}

/// Bind one line-group's members to its markers inner-first. Returns flags
/// parallel to `members`: true = exempted by a LINE marker (consumed).
fn peel_assign(members: &[usize], findings: &[crate::Finding], ctx: &mut LineMarkerCtx) -> Vec<bool> {
    let pairs = marker_inventory(members, findings, ctx.supps, ctx.used_line, ctx.taken);
    let mut ok = vec![false; members.len()];
    // members sort by col DESC (inner-first) while the inventory sorts by
    // line DESC — a single advancing pointer strands a marker whose kind
    // matches a LATER member behind an earlier non-match (review bot):
    // search the whole inventory for the first unused marker matching each
    // member's signal
    let mut used: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (j, &i) in members.iter().enumerate() {
        if let Some((pi, cand)) = pairs
            .iter()
            .enumerate()
            .find(|(pi, cand)| !used.contains(pi) && signal_matches(&cand.1, &findings[i].kind))
        {
            used.insert(pi);
            ctx.taken.insert(cand.clone());
            ctx.used_line.insert(cand.clone());
            ctx.spent.insert(cand.clone());
            ok[j] = true;
        }
    }
    ok
}

/// Filter findings through the suppressions + emit the why-less suppression
/// findings — the shared post-filter (Python `_scan_file`'s). `marker` is the
/// comment token ('#' or "//") used in the why-less messages. `pre_used`
/// carries the suppressions the caller's cc-array retain already honored so
/// stale detection does not re-flag them (the Rust layer's cc path).
pub fn apply_suppressions_impl(
    findings: Vec<crate::Finding>,
    comments: &[(usize, String)],
    file: &str,
    marker: &str,
    books: &mut SuppressionBooks,
) -> Vec<crate::Finding> {
    let supps = suppressions_from_comments(comments);
    let mut out = Vec::new();
    let mut used_line: std::collections::HashSet<(usize, String)> = books.pre_used.lines.clone();
    let mut used_file: std::collections::HashSet<String> = books.pre_used.files.clone();
    let mut seen_invalid: std::collections::HashSet<(usize, String)> = std::collections::HashSet::new();
    for (ln, entries) in &supps.line {
        for (sig, why) in entries {
            if why.is_empty() && seen_invalid.insert((*ln, sig.clone())) {
                out.push(crate::Finding {
col: 0,
                file: file.to_string(),
                line: *ln,
                function: String::new(),
                kind: "suppression".into(),
                severity: "fail".into(),
                message: format!(
                    "suppression '{marker} lucidlint: ignore {sig}' at line {ln} without a why — exemptions only apply with an explanation"
                ),
            });
            }
        }
    }
    for (sig, why) in &supps.file {
        if why.is_empty() {
            if let Some((ln, _)) = comments
                .iter()
                .find(|(_, t)| t.contains(&format!("lucidlint: ignore-file {sig}")))
            {
                out.push(crate::Finding {
col: 0,
                    file: file.to_string(),
                    line: *ln,
                    function: String::new(),
                    kind: "suppression".into(),
                    severity: "fail".into(),
                    message: format!(
                        "file suppression '{marker} lucidlint: ignore-file {sig}' at line {ln} without a why — exemptions only apply with an explanation"
                    ),
                });
            }
        }
    }
    // innermost-peel: markers bind inner-first, one marker per finding
    let mut exempted = vec![false; findings.len()];
    let mut taken: std::collections::HashSet<(usize, String)> = Default::default();
    for members in common_group_line_indices(&findings) {
        let mut ctx = LineMarkerCtx {
            supps: &supps,
            used_line: &mut used_line,
            taken: &mut taken,
            spent: books.spent,
        };
        let ok = peel_assign(&members, &findings, &mut ctx);
        for (j, &i) in members.iter().enumerate() {
            if ok[j] {
                exempted[i] = true;
                continue;
            }
            if suppress_track_file(&mut used_file, &supps, &findings[i]) {
                exempted[i] = true;
            }
        }
    }
    for (i, f) in findings.into_iter().enumerate() {
        if !exempted[i] {
            out.push(f);
        }
    }
    let ctx = StaleCtx {
        supps: &supps,
        used_line: &used_line,
        used_file: &used_file,
        comments,
        file,
        marker,
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
        if sig != &f.kind && signal_matches(sig, &f.kind) && !why.is_empty() {
            used_file.insert(sig.clone());
            return true;
        }
    }
    false
}

/// The stale-check context — the filter state + comment stream of one file.
struct StaleCtx<'a> {
    supps: &'a Suppressions,
    used_line: &'a std::collections::HashSet<(usize, String)>,
    used_file: &'a std::collections::HashSet<String>,
    comments: &'a [(usize, String)],
    file: &'a str,
    marker: &'a str,
}

impl<'a> StaleCtx<'a> {
    /// Stale suppressions: an explained suppression that matched nothing is
    /// dead weight — the signal it names no longer fires on that line/file
    /// (a family renamed, a finding fixed, a comment moved). Why-less ones
    /// are already findings; they never match by design.
    fn stale_suppression_findings(&self) -> Vec<crate::Finding> {
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
                out.push(crate::Finding {
                    col: 0,
                    file: self.file.to_string(),
                    line: *ln,
                    function: String::new(),
                    kind: "stale-suppression".into(),
                    severity: "fail".into(),
                    message: format!(
                        "suppression '{} lucidlint: ignore {sig}' at line {ln} no longer fires — remove it{fix_tail}",
                        self.marker
                    ),
                });
            }
        }
        for (sig, why) in &self.supps.file {
            if why.is_empty() || self.used_file.contains(sig) {
                continue;
            }
            if let Some((ln, _)) = self
                .comments
                .iter()
                .find(|(_, t)| t.contains(&format!("lucidlint: ignore-file {sig}")))
            {
                out.push(crate::Finding {
                    col: 0,
                    file: self.file.to_string(),
                    line: *ln,
                    function: String::new(),
                    kind: "stale-suppression".into(),
                    severity: "fail".into(),
                    message: format!(
                        "file suppression '{} lucidlint: ignore-file {sig}' no longer fires — remove it{fix_tail}",
                        self.marker
                    ),
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complexity_message_routes_by_shape() {
        // review: a dispatch chain's complexity suggestion must name the
        // registry, a rule battery the named checks — not a blind
        // extract-method (the directive stays extract-method: it is the real
        // auto-fix; the prose names the more lucid shape)
        let dispatch = complexity_message(36, "dispatch", "tool");
        assert!(dispatch.contains("dispatch chain over 'tool'"), "{dispatch}");
        assert!(dispatch.contains("HOIST THE LATENT DATA STRUCTURE"), "{dispatch}");
        assert!(dispatch.contains("lambda closures"), "{dispatch}");
        assert!(dispatch.contains("fix: dispatch-registry"), "{dispatch}");
        let rules = complexity_message(42, "rules", "violations");
        assert!(rules.contains("HOIST THE LATENT DATA STRUCTURE"), "{rules}");
        assert!(rules.contains("(condition, violation) table"), "{rules}");
        assert!(rules.contains("fix: rule-table"), "{rules}");
        let plain = complexity_message(20, "plain", "");
        assert!(
            plain.contains("extract part of this function into a named method"),
            "{plain}"
        );
    }
    use crate::Finding;

    fn finding(kind: &str, line: usize) -> Finding {
        Finding {
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
        let fs = apply_suppressions_impl(vec![finding("closures", 2)], &comments, "x.rs", "//", &mut books);
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
        let fs = apply_suppressions_impl(vec![], &comments, "x.rs", "//", &mut books);
        assert!(fs.iter().any(|f| f.kind == "stale-suppression"), "{:?}", fs);
    }
    #[test]
    fn stale_message_directive_only_where_a_fixer_exists() {
        // The `— fix: stale-suppression` directive must appear only where
        // the fix command can actually apply (the Python engine deletes the
        // marker; the Rust core has no such fixer) — an advertised fix that
        // cannot run is a false suggestion (user ruling).
        let comments = vec![(1, "// lucidlint: ignore latent-class nothing here".to_string())];
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let rs = apply_suppressions_impl(vec![], &comments, "x.rs", "//", &mut books);
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
        let py = apply_suppressions_impl(vec![], &comments, "x.py", "#", &mut books);
        let msg = py
            .iter()
            .find(|f| f.kind == "stale-suppression")
            .unwrap()
            .message
            .clone();
        assert!(msg.contains("fix: stale-suppression"), "{msg}");
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
        let fs = apply_suppressions_impl(vec![finding("partition", 9)], &comments, "x.rs", "//", &mut books);
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
        let fs = apply_suppressions_impl(vec![finding("strewing", 2)], &comments, "x.py", "#", &mut books);
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
            col: 5,
            ..finding("magic-number", 2)
        };
        let fs = apply_suppressions_impl(
            vec![deep_magic, finding("middle-man", 2)],
            &comments,
            "x.py",
            "#",
            &mut books,
        );
        assert!(
            !fs.iter().any(|f| f.kind == "magic-number" || f.kind == "middle-man"),
            "both markers must bind regardless of order skew: {:?}",
            fs
        );
    }

    #[test]
    fn decorator_line_does_not_break_suppression_window() {
        // B7: a comment two lines above the finding (a decorator line
        // intervenes) still suppresses — the window is 3 lines, not
        // line/line-1.
        let comments = vec![(1, "// lucidlint: ignore magic-number the gate threshold".to_string())];
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let fs = apply_suppressions_impl(vec![finding("magic-number", 3)], &comments, "x.rs", "//", &mut books);
        assert!(!fs.iter().any(|f| f.kind == "magic-number"), "{:?}", fs);
        assert!(!fs.iter().any(|f| f.kind == "stale-suppression"), "{:?}", fs);
    }

    #[test]
    fn window_is_bounded_far_comment_does_not_suppress() {
        // B7 guard: the window stays adjacent — a comment 4+ lines above is
        // NOT a suppression of the finding.
        let comments = vec![(1, "// lucidlint: ignore magic-number far away".to_string())];
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let fs = apply_suppressions_impl(vec![finding("magic-number", 5)], &comments, "x.rs", "//", &mut books);
        assert!(fs.iter().any(|f| f.kind == "magic-number"), "{:?}", fs);
    }
}

/// The report header — printed on every CLI run (text banner AND a `header`
/// field in --json; never under the LSP). It states the AIM so agents read
/// the intent before the findings: fix rather than suppress, and make every
/// suppression reviewer-checkable.
pub const REPORT_HEADER: &str = "lucidlint - the aim is code that is readable, maintainable, and obviously correct: fix findings instead of suppressing them, and give every suppression a why a reviewer can check";

#[cfg(test)]
mod header_tests {
    use super::REPORT_HEADER;

    #[test]
    fn report_header_names_the_three_aims() {
        assert!(REPORT_HEADER.contains("readable"));
        assert!(REPORT_HEADER.contains("maintainable"));
        assert!(REPORT_HEADER.contains("obviously correct"));
        assert!(REPORT_HEADER.contains("why a reviewer can check"));
    }
}

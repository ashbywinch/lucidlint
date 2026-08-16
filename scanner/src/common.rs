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
use std::hash::{Hash, Hasher};

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

/// A function is a duplicate candidate when it has real body substance:
/// at least 2 non-doc statements and a skeleton of at least 12 tokens.
/// Pure; each layer supplies its own statement count (docstring filtering
/// is Python-shaped).
pub fn is_duplicate_size(skeleton_len: usize, non_doc_stmts: usize) -> bool {
    non_doc_stmts >= 2 && skeleton_len >= 12
}

/// Order-independent content hash of a skeleton's bigram set — identical
/// sets collide, so the hash IS the dice=1.0 test. XOR keeps it order-free;
/// DefaultHasher::new() has fixed keys, so the value is deterministic.
pub fn bigram_set_hash(t: &[String]) -> u64 {
    let mut h = 0u64;
    if t.len() >= 2 {
        for w in t.windows(2) {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            w[0].hash(&mut hasher);
            w[1].hash(&mut hasher);
            h ^= hasher.finish();
        }
    }
    h
}

/// Dice coefficient over bigram sets — the language-neutral similarity.
pub fn dice_similarity(a: &[String], b: &[String]) -> f64 {
    let bigrams =
        |t: &[String]| -> Vec<(String, String)> { t.windows(2).map(|w| (w[0].clone(), w[1].clone())).collect() };
    let (ab, bb) = (bigrams(a), bigrams(b));
    if ab.is_empty() && bb.is_empty() {
        return 0.0; // matches the Python reference: no shared bigrams, never a duplicate
    }
    if ab.is_empty() || bb.is_empty() {
        return 0.0;
    }
    let mut common = 0usize;
    let mut bseen: Vec<bool> = vec![false; bb.len()];
    for x in &ab {
        for (j, y) in bb.iter().enumerate() {
            if !bseen[j] && x == y {
                common += 1;
                bseen[j] = true;
                break;
            }
        }
    }
    (2.0 * common as f64) / (ab.len() + bb.len()) as f64
}

// --------------------------------------------------------------------------- suppressions

/// One line-level suppression and the (signal, why) parsed from it.
/// Line suppressions exempt a finding on that line or the line before;
/// file suppressions exempt the signal anywhere in the file (with a why).
pub struct Suppressions {
    pub line: HashMap<usize, (String, String)>,
    pub file: HashMap<String, String>,
}

/// Parse `code-health: ignore <signal> <why>` / `ignore-file` comments.
/// `comments` are (line, full comment text incl. the marker) — each language
/// layer extracts them its own way (ruff tokens for Python, a string-aware
/// scan for Rust); the parse and the matching are shared.
pub fn suppressions_from_comments(comments: &[(usize, String)]) -> Suppressions {
    let mut line_map = HashMap::new();
    let mut file_map = HashMap::new();
    for (ln, text) in comments {
        let trimmed = text.trim_start_matches(['#', '/']).trim_start();
        if let Some(rest) = trimmed.strip_prefix("code-health: ignore-file ") {
            let mut it = rest.splitn(2, char::is_whitespace);
            let signal = it.next().unwrap_or("").to_string();
            let why = it.next().unwrap_or("").trim().to_string();
            if !signal.is_empty() {
                file_map.insert(signal, why);
            }
        } else if let Some(rest) = trimmed.strip_prefix("code-health: ignore ") {
            let mut it = rest.splitn(2, char::is_whitespace);
            let signal = it.next().unwrap_or("").to_string();
            let why = it.next().unwrap_or("").trim().to_string();
            if !signal.is_empty() {
                line_map.insert(*ln, (signal, why));
            }
        }
    }
    Suppressions {
        line: line_map,
        file: file_map,
    }
}

/// The Python `_suppressed`: a finding is exempt when its line or line-1
/// carries an explained suppression for that signal.
pub fn suppressed(signal: &str, line: usize, supps: &Suppressions) -> bool {
    for ln in [line, line.saturating_sub(1)] {
        if let Some((sig, why)) = supps.line.get(&ln) {
            if sig == signal && !why.is_empty() {
                return true;
            }
        }
    }
    false
}

/// Suppressions the caller's own filtering paths already honored (the Rust
/// cc-array retain removes complexity findings before this filter runs) —
/// stale detection must not re-flag them.
#[derive(Default)]
pub struct PreUsedSuppressions {
    pub lines: std::collections::HashSet<(usize, String)>,
    pub files: std::collections::HashSet<String>,
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
    pre_used: &PreUsedSuppressions,
) -> Vec<crate::Finding> {
    let supps = suppressions_from_comments(comments);
    let mut out = Vec::new();
    let mut used_line: std::collections::HashSet<(usize, String)> = pre_used.lines.clone();
    let mut used_file: std::collections::HashSet<String> = pre_used.files.clone();
    // the Python tool dedups suppressions by line (one per line)
    let mut seen_invalid: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (ln, (sig, why)) in &supps.line {
        if why.is_empty() && seen_invalid.insert(*ln) {
            out.push(crate::Finding {
                file: file.to_string(),
                line: *ln,
                function: String::new(),
                kind: "suppression".into(),
                severity: "fail".into(),
                message: format!(
                    "suppression '{marker} code-health: ignore {sig}' at line {ln} without a why — exemptions only apply with an explanation"
                ),
            });
        }
    }
    for (sig, why) in &supps.file {
        if why.is_empty() {
            if let Some((ln, _)) = comments
                .iter()
                .find(|(_, t)| t.contains(&format!("code-health: ignore-file {sig}")))
            {
                out.push(crate::Finding {
                    file: file.to_string(),
                    line: *ln,
                    function: String::new(),
                    kind: "suppression".into(),
                    severity: "fail".into(),
                    message: format!(
                        "file suppression '{marker} code-health: ignore-file {sig}' at line {ln} without a why — exemptions only apply with an explanation"
                    ),
                });
            }
        }
    }
    for f in findings {
        if suppress_track_line(&mut used_line, &supps, &f) {
            continue;
        }
        if suppress_track_file(&mut used_file, &supps, &f) {
            continue; // ignore-file with a why exempts; a why-less one does not
        }
        out.push(f);
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

/// Was this finding exempted by an explained line suppression? Records the
/// matched comment so the stale check does not re-flag it.
fn suppress_track_line(
    used_line: &mut std::collections::HashSet<(usize, String)>,
    supps: &Suppressions,
    f: &crate::Finding,
) -> bool {
    for ln in [f.line, f.line.saturating_sub(1)] {
        if let Some((sig, why)) = supps.line.get(&ln) {
            if sig == &f.kind && !why.is_empty() {
                used_line.insert((ln, sig.clone()));
                return true;
            }
        }
    }
    false
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
        for (ln, (sig, why)) in &self.supps.line {
            if why.is_empty() || self.used_line.contains(&(*ln, sig.clone())) {
                continue;
            }
            out.push(crate::Finding {
                file: self.file.to_string(),
                line: *ln,
                function: String::new(),
                kind: "stale-suppression".into(),
                severity: "fail".into(),
                message: format!(
                    "suppression '{} code-health: ignore {sig}' at line {ln} no longer fires — remove it",
                    self.marker
                ),
            });
        }
        for (sig, why) in &self.supps.file {
            if why.is_empty() || self.used_file.contains(sig) {
                continue;
            }
            if let Some((ln, _)) = self
                .comments
                .iter()
                .find(|(_, t)| t.contains(&format!("code-health: ignore-file {sig}")))
            {
                out.push(crate::Finding {
                    file: self.file.to_string(),
                    line: *ln,
                    function: String::new(),
                    kind: "stale-suppression".into(),
                    severity: "fail".into(),
                    message: format!(
                        "file suppression '{} code-health: ignore-file {sig}' no longer fires — remove it",
                        self.marker
                    ),
                });
            }
        }
        out
    }
}

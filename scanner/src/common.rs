// lucidlint: ignore-file long-param-list the suppression pass threads the scan shapes and the
// evidence flags (findings, comments, file, marker, books, numeric-literal probe) —
// an options struct would add a hop for the two callers
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
    /// "module" | "class" | "module-class" (a module whose non-import
    /// top-levels are exactly one class def — pairs as both).
    pub entity: &'static str,
    /// The class identity's direct base names ("Name:Base" / "Attr:mod:Base")
    /// — empty for module/module-class identities. C2: two classes sharing
    /// one base get the subclass message, not the fork prose.
    pub bases: Vec<String>,
}

/// A module-level function whose body is exactly `return <callee>(<all
/// params>)` — the middle edge of a forwarding chain (#30).
#[derive(Clone)]
pub struct ForwarderFn {
    pub rel: String,
    pub name: String,
    pub line: usize,
    pub params: Vec<String>,
    /// "Fn:<name>" for a module-fn callee, "Method:<recv-param>:<method>" for
    /// a method callee on another class.
    pub callee: String,
}

/// A class method whose body is exactly `return F(<all params>)` with F a
/// module-level function — the head of a forwarding chain (#30).
#[derive(Clone)]
pub struct ForwarderMethod {
    pub rel: String,
    pub class: String,
    pub name: String,
    pub line: usize,
    pub params: Vec<String>, // minus self/cls
    pub fn_callee: String,
}

/// Every method of every top-level class (params minus receiver) — the
/// terminal pool the forwarding-chain detector resolves `recv.method(...)`
/// callees against (#30, R9).
#[derive(Clone)]
pub struct RepoMethod {
    pub rel: String,
    pub class: String,
    pub name: String,
    pub line: usize,
    pub params: Vec<String>,
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
            "cyclomatic complexity {cc} (>= 15) — the function is a dispatch chain over '{detail}': every arm is a named handler — HOIST THE HIDDEN DATA STRUCTURE: the chain IS a (selector → action) table — collapse it into a dict of {detail} → lambda closures in Python (a match in Rust), and dispatch by lookup — fix: dispatch-registry (previews the table; apply with --confirm)"
        ),
        "rules" => format!(
            "cyclomatic complexity {cc} (>= 15) — the function is a battery of independent checks each appending to '{detail}' — HOIST THE HIDDEN DATA STRUCTURE: the if/append chain IS a (condition, violation) table — collapse it into a list of such pairs whose conditions are lambdas (Python) or fn pointers (Rust), and collect the violations whose condition holds — fix: rule-table (previews the table; apply with --confirm)"
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
/// scan for Rust); the parse and the matching are shared. A marker binds by
/// its signal name alone: the why may truncate at line end (a repo marker
/// cut mid-expression still binds — G5); only a marker with NO why at all
/// is why-less and does not bind.
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

// (P2) ————————————————— signature-window extension

/// (P2) The nearest preceding line whose STRIPPED text opens a def (`def `
/// or `async def `) — the signature's anchor. The search is bounded: a
/// finding in a signature lies at most a signature-span below the def, so
/// a def further up cannot be the enclosing signature.
fn def_line_before(source: &str, line: usize) -> Option<usize> {
    let lines: Vec<&str> = source.lines().collect();
    let mut l = line.saturating_sub(1); // 0-based scan start
    let bound = l.saturating_sub(40);
    while l > bound {
        l -= 1;
        let t = lines.get(l).copied().unwrap_or("").trim_start();
        if t.starts_with("def ") || t.starts_with("async def ") {
            return Some(l + 1);
        }
    }
    None
}

/// (P2) Does the line's CODE (trailing comment stripped) end with `:`?
fn code_ends_with_colon(t: &str) -> bool {
    let code = match t.find('#') {
        Some(i) => &t[..i],
        None => t,
    };
    code.trim_end().ends_with(':')
}

/// (P2) The line where a def's signature closes: the first line at or after
/// `def_line` where the paren depth (opened by `def name(`) returns to 0 on
/// a line whose code ends with `:`. A single-line def closes on its own
/// line. None when no closing colon appears within the bound — then the
/// finding cannot be proven to sit in a signature.
fn closing_colon_line(source: &str, def_line: usize) -> Option<usize> {
    let lines: Vec<&str> = source.lines().collect();
    let mut depth: i32 = 0;
    let max = (def_line + 40).min(lines.len());
    for l in def_line..=max {
        let t = lines.get(l - 1).copied().unwrap_or("");
        depth += t.chars().filter(|&c| c == '(').count() as i32;
        depth -= t.chars().filter(|&c| c == ')').count() as i32;
        if depth <= 0 && code_ends_with_colon(t) {
            return Some(l);
        }
    }
    None
}

/// (P2) The def line anchoring a signature finding, if any: the finding
/// must lie between the nearest preceding def and its closing-colon line
/// (a parameter annotation line or the return-annotation/colon line).
/// Body findings sit after the colon and get no extension. Returns the
/// def line once — the window builder needs it and the membership test
/// would otherwise compute the upward scan twice per finding.
fn signature_def_line(source: &str, line: usize) -> Option<usize> {
    let def = def_line_before(source, line)?;
    match closing_colon_line(source, def) {
        Some(col) if line >= def && line <= col => Some(def),
        None if line == def => Some(def),
        _ => None,
    }
}

/// (P2) A signature finding's binding window: the parameter-anchored
/// 3-line window UNION the def-above pair [def-1..def] — the codebase puts
/// the suppression one line above the def, so a multi-line signature's
/// parameter-anchored findings must not strand that marker outside the
/// window. Descending order keeps the parameter-adjacent marker priority
/// (a marker on the finding's own line wins). Non-signature findings keep
/// the plain window.
fn finding_window_lines(source: &str, line: usize) -> Vec<usize> {
    let mut out: Vec<usize> = window_lines(line).collect();
    if let Some(def) = signature_def_line(source, line) {
        for l in (def.saturating_sub(1)..=def).rev() {
            if l >= 1 && !out.contains(&l) {
                out.push(l);
            }
        }
    }
    out
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

/// Marker inventory for one line-group: every explained marker line whose
/// signal matches any member, one pair per line. A pair another line-group
/// consumed is NOT filtered here — a marker two lines above a def binds the
/// def-anchored finding AND the literal one line below it (G3): one marker
/// covers the whole def site, recorded spent once (set semantics). The
/// window is the extended signature window (P2) for def-anchored findings.
fn marker_inventory(
    members: &[usize],
    findings: &[crate::Finding],
    supps: &Suppressions,
    source: &str,
) -> Vec<(usize, String)> {
    let mut pairs: Vec<(usize, String)> = Vec::new();
    for ln in finding_window_lines(source, findings[members[0]].line) {
        if let Some(entries) = supps.line.get(&ln) {
            for (sig, why) in entries {
                if why.is_empty() || pairs.iter().any(|(pl, _)| *pl == ln) {
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
    source: &'a str,
    used_line: &'a mut std::collections::HashSet<(usize, String)>,
    taken: &'a mut std::collections::HashSet<(usize, String)>,
    spent: &'a mut std::collections::HashSet<(usize, String)>,
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
    fn peel(&mut self, members: &[usize], findings: &[crate::Finding]) -> Vec<bool> {
        let pairs = marker_inventory(members, findings, self.supps, self.source);
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
            let matches_any = |pi: usize| {
                js.iter()
                    .any(|&j| signal_matches(&pairs[pi].1, &findings[members[j]].kind))
            };
            if let Some(pi) = (0..pairs.len()).find(|&pi| !used.contains(&pi) && matches_any(pi)) {
                used.insert(pi);
                let cand = pairs[pi].clone();
                self.taken.insert(cand.clone());
                self.used_line.insert(cand.clone());
                self.spent.insert(cand.clone());
                for &j in &js {
                    ok[j] = true;
                }
            }
        }
        ok
    }
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
pub fn apply_suppressions_impl(
    findings: Vec<crate::Finding>,
    source: &str,
    comments: &[(usize, String)],
    file: &str,
    marker: &str,
    books: &mut SuppressionBooks,
    magic_exempt_labels: &[&'static str],
) -> Vec<crate::Finding> {
    let supps = suppressions_from_comments(comments);
    let mut out = Vec::new();
    let mut used_line: std::collections::HashSet<(usize, String)> = books.pre_used.lines.clone();
    let mut used_file: std::collections::HashSet<String> = books.pre_used.files.clone();
    let mut seen_invalid: std::collections::HashSet<(usize, String)> = std::collections::HashSet::new();
    for (ln, entries) in &supps.line {
        for (sig, why) in entries {
            if why.is_empty() && seen_invalid.insert((*ln, sig.clone())) {
                out.push(crate::Finding { seam_members: Vec::new(), col: 0,
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
                out.push(crate::Finding { seam_members: Vec::new(), col: 0,
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
    let by_signal = signal_line_index(&findings);
    for members in common_group_line_indices(&findings) {
        let mut ctx = LineMarkerCtx {
            supps: &supps,
            source,
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
    /// Every finding's (kind, lines) that fired in this scan — the facts
    /// the stale binding-reason reasons cite (gone / covered / out of
    /// window).
    by_signal: &'a std::collections::HashMap<String, Vec<usize>>,
    /// G4: the exemption-kind labels among the file's still-present
    /// magic-number candidates (unit-named, constant-definition,
    /// data-table, length-guard, position/index) — the stale message names
    /// exactly the kinds that cover the file's literals, never a blanket
    /// unit/constant/table claim for a file whose literals are indices.
    magic_exempt_labels: &'a [&'static str],
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
                // "stale — remove it" means exactly one case: the signal
                // fires NOWHERE in this file. When the family still fires
                // anywhere, the marker is documentation (a mis-placed or
                // covered marker binds nothing) — no stale verdict, and no
                // advice to move it (plan Phase 3).
                let Some(reason) = self.stale_reason(sig) else {
                    continue;
                };
                let delta = self.exemption_delta(sig);
                out.push(crate::Finding { seam_members: Vec::new(), col: 0,
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
            if let Some((ln, _)) = self
                .comments
                .iter()
                .find(|(_, t)| t.contains(&format!("lucidlint: ignore-file {sig}")))
            {
                // same fires-nowhere gate as the line markers: a file
                // suppression over a family that still fires is either bound
                // or redundant documentation — never a stale verdict
                if self.lines_for(sig).is_empty() {
                    let delta = self.exemption_delta(sig);
                    out.push(crate::Finding { seam_members: Vec::new(), col: 0,
                    file: self.file.to_string(),
                    line: *ln,
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
            .filter(|(kind, _)| signal_matches(sig, kind))
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
        assert!(dispatch.contains("HOIST THE HIDDEN DATA STRUCTURE"), "{dispatch}");
        assert!(dispatch.contains("lambda closures"), "{dispatch}");
        assert!(dispatch.contains("fix: dispatch-registry"), "{dispatch}");
        let rules = complexity_message(42, "rules", "violations");
        assert!(rules.contains("HOIST THE HIDDEN DATA STRUCTURE"), "{rules}");
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
        let fs = apply_suppressions_impl(
            vec![finding("closures", 2)],
            "",
            &comments,
            "x.rs",
            "//",
            &mut books,
            &[],
        );
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
        let fs = apply_suppressions_impl(vec![], "", &comments, "x.rs", "//", &mut books, &[]);
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
        let rs = apply_suppressions_impl(vec![], "", &comments, "x.rs", "//", &mut books, &[]);
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
        let py = apply_suppressions_impl(vec![], "", &comments, "x.py", "#", &mut books, &[]);
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
        let gone = apply_suppressions_impl(
            vec![],
            "",
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
        let bare = apply_suppressions_impl(
            vec![],
            "",
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
        let misplaced = apply_suppressions_impl(
            vec![finding("magic-number", 3)],
            "",
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
        let covered = apply_suppressions_impl(
            vec![finding("magic-number", 3)],
            "",
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
        // The fires-nowhere distinction, per signal: a marker one line past
        // the window over a family firing below (rightmove_url:57 — marker
        // at 3, window of line 6 is 4..=6) gets no verdict and no advice;
        // the SAME marker would be stale if the family fired nowhere at all.
        let mut spent = std::collections::HashSet::new();
        let near = apply_suppressions_impl(
            vec![finding("magic-number", 6), finding("magic-number", 9)],
            "",
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
        let far = apply_suppressions_impl(
            vec![],
            "",
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
        let index_only = apply_suppressions_impl(
            vec![],
            "",
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
        let none = apply_suppressions_impl(
            vec![],
            "",
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
        let fs = apply_suppressions_impl(
            vec![finding("partition", 9)],
            "",
            &comments,
            "x.rs",
            "//",
            &mut books,
            &[],
        );
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
        let fs = apply_suppressions_impl(
            vec![finding("strewing", 2)],
            "",
            &comments,
            "x.py",
            "#",
            &mut books,
            &[],
        );
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
            seam_members: Vec::new(),
            col: 5,
            ..finding("magic-number", 2)
        };
        let fs = apply_suppressions_impl(
            vec![deep_magic, finding("middle-man", 2)],
            "",
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
    fn decorator_line_does_not_break_suppression_window() {
        // B7: a comment two lines above the finding (a decorator line
        // intervenes) still suppresses — the window is 3 lines, not
        // line/line-1.
        let comments = vec![(1, "// lucidlint: ignore magic-number the gate threshold".to_string())];
        let mut books = SuppressionBooks {
            pre_used: &PreUsedSuppressions::default(),
            spent: &mut std::collections::HashSet::new(),
        };
        let fs = apply_suppressions_impl(
            vec![finding("magic-number", 3)],
            "",
            &comments,
            "x.rs",
            "//",
            &mut books,
            &[],
        );
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
        let fs = apply_suppressions_impl(
            vec![finding("magic-number", 5)],
            "",
            &comments,
            "x.rs",
            "//",
            &mut books,
            &[],
        );
        assert!(fs.iter().any(|f| f.kind == "magic-number"), "{:?}", fs);
    }
}

/// The report header — printed on every CLI run (text banner AND a `header`
/// field in --json; never under the LSP). It states the AIM so agents read
/// the intent before the findings: findings are pointers, fix rather than
/// suppress, and make every suppression reviewer-checkable (Fourth-pass).
pub const REPORT_HEADER: &str = "lucidlint — the aim is code that is maintainable, lucid, and obviously correct. Findings and suggested fixes are pointers, not orders: for each, judge the best way to make this code more maintainable, lucid, and obviously correct. Fix findings instead of suppressing them; give every suppression a why a reviewer can check";

#[cfg(test)]
mod header_tests {
    use super::REPORT_HEADER;

    #[test]
    fn report_header_names_the_three_aims() {
        assert!(REPORT_HEADER.contains("lucid"));
        assert!(REPORT_HEADER.contains("obviously correct"));
        assert!(REPORT_HEADER.contains("why a reviewer can check"));
        assert!(REPORT_HEADER.contains("pointers, not orders"));
    }
}

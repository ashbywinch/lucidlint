//! `flat-folder`: a directory holding 20+ browsable files — it stops telling a
//! reader which files belong together.

use crate::finding::Finding;

/// `flat-folder`: the rule object. Its methods read one directory's names; the evidence and the message live together.
pub struct FlatFolder;

/// A directory whose browsable files (modules + docs) cross the threshold —
/// the material of a structure with no structure. Refs are repo-relative;
/// the caller filters `__init__.py`, vendored and ignored dirs, so this is a
/// pure count over what the reader actually browses.
pub const FLAT_FOLDER_THRESHOLD: usize = 20;

impl FlatFolder {
    pub fn findings(modules: &[String], docs: &[String]) -> Vec<Finding> {
        use std::collections::BTreeMap;
        // per dir: (module rels, doc rels) — the names are the evidence for which
        // domain subsets the folder can be split into
        let mut per_dir: BTreeMap<String, (Vec<&String>, Vec<&String>)> = BTreeMap::new();
        for rel in modules {
            if rel.rsplit('/').next() == Some("__init__.py") {
                continue; // a package marker, not a module the reader opens
            }
            let dir = rel.rsplit_once('/').map_or("", |(d, _)| d);
            per_dir.entry(dir.to_string()).or_default().0.push(rel);
        }
        for rel in docs {
            let dir = rel.rsplit_once('/').map_or("", |(d, _)| d);
            per_dir.entry(dir.to_string()).or_default().1.push(rel);
        }
        let mut out = Vec::new();
        for (dir, (mods, txt)) in per_dir {
            if mods.len() + txt.len() < FLAT_FOLDER_THRESHOLD {
                continue;
            }
            let label = if dir.is_empty() { "the repo root" } else { dir.as_str() };
            // the action names what the folder holds: a docs pile has no modules
            // to group (round: "'docs' holds 31 docs … group the modules")
            let action = match (mods.len(), txt.len()) {
                (0, _) => "Split it into coherent topic subsets: one subfolder per topic these pages are about, named with the topic its pages share.",
                (_, 0) => "Split it into coherent domain subsets: one subfolder per thing these modules are about, named with the noun its members share.",
                _ => "Split it into coherent domain subsets: one subfolder per thing these files are about, named with the noun its members share.",
            };
            // the candidates the names already offer — the action's evidence, and
            // the antidote to `misc/` or `1/`: a subset is a thing, not a bucket
            let names: Vec<&String> = mods.iter().chain(txt.iter()).copied().collect();
            let candidates = Self::subset_candidates(&names, &dir);
            let advice = if candidates.is_empty() {
                String::new()
            } else {
                format!(
                    " Its names already repeat: {candidates} — check that each one names a thing before moving the files."
                )
            };
            out.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                col: 0,
                file: if dir.is_empty() { ".".to_string() } else { dir.clone() },
                line: 0,
                function: String::new(),
                kind: "flat-folder".into(),
                severity: "warn".into(),
                message: format!(
                    "'{label}' holds {} files ({m} modules, {t} docs) — at {FLAT_FOLDER_THRESHOLD}+ files one folder stops telling a reader which belong together. {action}{advice} A split by file kind, size, or number is not a subset: it names the files, not the thing they belong to.",
                    mods.len() + txt.len(),
                    m = mods.len(),
                    t = txt.len()
                ),
            });
        }
        out
    }

    /// The lower-case tokens a filename carries: split on separators
    /// (`_`, `-`, `.`) AND CamelCase boundaries, so `CommuteNode`,
    /// `commute.node` and `commute_node` all offer `commute` and `node`.
    fn name_tokens(stem: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for piece in stem.split(|c: char| !c.is_alphanumeric()) {
            let mut start = 0usize;
            let chars: Vec<char> = piece.chars().collect();
            for (i, c) in chars.iter().enumerate() {
                let boundary = i > start && c.is_uppercase() && chars[i - 1].is_lowercase();
                if boundary || i + 1 == chars.len() {
                    let end = if i + 1 == chars.len() { i + 1 } else { i };
                    let token: String = chars[start..end].iter().collect::<String>().to_lowercase();
                    if token.len() > 2 {
                        out.push(token);
                    }
                    start = i;
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The nouns the folder's filenames already repeat: tokens shared by >= 3
    /// files and by less than 60% of them (a token in most files is the folder's
    /// own family — `node` in `nodes/` — not a subset). Up to three, most common
    /// first. These are candidates for a reader to check, never orders.
    fn subset_candidates(names: &[&String], dir: &str) -> String {
        use std::collections::BTreeMap;
        // The folder's own name is the family's name — `nodes/`, `apis/`, `bus/` —
        // never one of its subsets. Plain and plural forms of the same word are
        // matched by suffix, so a singular folder (`bus`) is not singularised into
        // nonsense (`bu`).
        let family = dir.rsplit('/').next().unwrap_or(dir).to_lowercase();
        let is_family =
            |token: &str| token == family || format!("{token}s") == family || format!("{token}es") == family;
        let mut per_token: BTreeMap<String, usize> = BTreeMap::new();
        for rel in names {
            let stem = rel.rsplit('/').next().unwrap_or(rel);
            let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
            for token in Self::name_tokens(stem) {
                *per_token.entry(token).or_default() += 1;
            }
        }
        let limit = (names.len() as f64 * 0.6).max(3.0) as usize;
        let mut candidates: Vec<(String, usize)> = per_token
            .into_iter()
            .filter(|(token, n)| *n >= 3 && *n < limit && !is_family(token))
            .collect();
        candidates.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        candidates
            .iter()
            .take(3)
            .map(|(token, n)| format!("{token} ({n})"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

//! The repo config (`.lucidlint.toml` / `[tool.lucidlint]`) — loaded by the
//! Rust core so the LSP applies the SAME silencing rules as the gate.
//! Today config-ignores are Python-orchestrator-only: the LSP flags
//! config-ignored findings the gate hides, which broke the agent's trust in
//! the LSP (it concluded the LSP was ignoring its silencing rules).

use std::collections::HashSet;
use std::path::Path;

/// The rule groups the config's `group:` prefix expands — the mirror of
/// lucidlint.py's RULE_GROUPS (drift is pinned by tests/test_config_parity.py).
pub fn rule_groups() -> &'static [(&'static str, &'static [&'static str])] {
    &[
        (
            "architecture",
            &[
                "complexity",
                "large-function",
                "closures",
                "partition",
                "strewing",
                "record-shape",
                "duplicate",
                "layer-mix",
                "folder-mix",
                "hub-file",
                "high-risk",
                "hotspot",
                "over-abstraction",
                "long-param-list",
                "churn-untested",
                "detached-method",
            ],
        ),
        (
            "style",
            &[
                "magic-number",
                "noop-statement",
                "unreachable",
                "vague-name",
                "class-module",
                "builtin-shadow",
                "broad-except",
                "swallow",
                "inline-import",
                "private-import",
                "global-state",
                "unused",
                "import-cycle",
                "docs-link",
                "docs-undiscoverable",
                "boolean-arg",
                "debug-artifact",
                "positional-literals",
                "guard-clauses",
                "latent-visitor",
                "conditional-polymorphism",
                "special-case",
                "middle-man",
                "unused-setter",
                "loop-pipeline",
            ],
        ),
        (
            "test-discipline",
            &["monkeypatch", "skipif", "fakefs", "no-assert-test"],
        ),
        (
            "suppression",
            &[
                "suppression",
                "type-ignore",
                "allow-reason",
                "noqa",
                "stale-suppression",
            ],
        ),
    ]
}

/// Expand a config table's `ignore`/`ignored_signals` list (with `group:`
/// prefix expansion) into the set of silenced signals.
fn expand_ignores(raw: &toml::Value) -> HashSet<String> {
    let mut out = HashSet::new();
    let Some(list) = raw.get("ignore").or_else(|| raw.get("ignored_signals")) else {
        return out;
    };
    let Some(items) = list.as_array() else {
        return out;
    };
    for item in items {
        let Some(name) = item.as_str() else {
            continue;
        };
        if let Some(group) = name.strip_prefix("group:") {
            for (gname, kinds) in rule_groups() {
                if *gname == group {
                    out.extend(kinds.iter().map(|k| k.to_string()));
                }
            }
        } else {
            out.insert(name.trim().to_string());
        }
    }
    out
}

/// The repo's silencing config — global and per-path ignored signals.
#[derive(Clone, Default)]
pub struct LucidConfig {
    global: HashSet<String>,
    per_path: Vec<(String, HashSet<String>)>,
}

impl LucidConfig {
    /// Is `signal` silenced for the repo-relative path `rel`? Same semantics
    /// as the gate: global ignore, then per-path `Path.match` globs.
    pub fn is_ignored(&self, signal: &str, rel: &str) -> bool {
        if self.global.contains(signal) {
            return true;
        }
        for (pattern, ignored) in &self.per_path {
            if ignored.contains(signal) && pure_path_match(rel, pattern) {
                return true;
            }
        }
        false
    }
}

/// Load `.lucidlint.toml` `[lucidlint]`, falling back to `pyproject.toml`
/// `[tool.lucidlint]` — the same precedence as the gate.
pub fn load_lucidlint_config(root: &Path) -> LucidConfig {
    let mut config = LucidConfig::default();
    let (text, key_path) = match std::fs::read_to_string(root.join(".lucidlint.toml")) {
        Ok(t) => (t, vec!["lucidlint"]),
        Err(_) => {
            let Ok(t) = std::fs::read_to_string(root.join("pyproject.toml")) else {
                return config;
            };
            (t, vec!["tool", "lucidlint"])
        }
    };
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return config;
    };
    let mut current = &value;
    for key in &key_path {
        let Some(next) = current.get(key) else {
            return config;
        };
        current = next;
    }
    config.global = expand_ignores(current);
    let Some(table) = current.as_table() else {
        return config;
    };
    for (key, val) in table {
        if key == "ignore" || key == "ignored_signals" {
            continue;
        }
        let Some(inner) = val.as_table() else {
            continue;
        };
        let ignored = expand_ignores(&toml::Value::Table(inner.clone()));
        if !ignored.is_empty() {
            config.per_path.push((key.clone(), ignored));
        }
    }
    config
}

/// PurePath.match semantics (the gate's `Path(a.file).match(pattern)`):
/// right-aligned fnmatch per part; `**` is a single `*` part; extra leading
/// path parts are ignored. Replicated bug-for-bug so the LSP agrees with the
/// gate (the semantics are pinned by tests/test_config_parity.py).
fn pure_path_match(rel: &str, pattern: &str) -> bool {
    let rel_parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    let pat_parts: Vec<&str> = pattern.split('/').filter(|p| !p.is_empty()).collect();
    if pat_parts.len() > rel_parts.len() {
        return false;
    }
    let offset = rel_parts.len() - pat_parts.len();
    pat_parts
        .iter()
        .enumerate()
        .all(|(i, p)| fnmatch_part(rel_parts[offset + i], p))
}

/// The stdlib-fnmatch subset the config globs use: `*` any chars, `?` one,
/// `[a-z]`/`[!a-z]` classes. `**` behaves as `*` (Python's PurePath.match
/// treats it as one fnmatch part).
fn fnmatch_part(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();

    fn rec(t: &[char], p: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => {
                let next = p.iter().position(|c| *c != '*').map(|i| i + 1).unwrap_or(p.len());
                (0..=t.len()).any(|i| rec(&t[i..], &p[next..]))
            }
            Some('?') => !t.is_empty() && rec(&t[1..], &p[1..]),
            Some('[') => {
                let (matched, rest_p, rest_t) = match_class(t, p);
                matched && rec(rest_t, rest_p)
            }
            Some(c) => !t.is_empty() && t[0] == *c && rec(&t[1..], &p[1..]),
        }
    }
    rec(&t, &p)
}

/// A `[..]` class: consume it from the pattern; test the first text char.
/// Returns (matched, remaining pattern, remaining text).
fn match_class<'a>(t: &'a [char], p: &'a [char]) -> (bool, &'a [char], &'a [char]) {
    let mut i = 1usize;
    let negate = p.get(i).is_some_and(|c| *c == '!' || *c == '^');
    if negate {
        i += 1;
    }
    let mut any = false;
    while i < p.len() && p[i] != ']' {
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            if let Some(c) = t.first() {
                if *c >= p[i] && *c <= p[i + 2] {
                    any = true;
                }
            }
            i += 3;
        } else {
            if t.first() == Some(&p[i]) {
                any = true;
            }
            i += 1;
        }
    }
    let rest_p = if i < p.len() { &p[i + 1..] } else { &p[p.len()..] };
    if t.is_empty() {
        return (false, p, t);
    }
    (any != negate, rest_p, &t[1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_match_mirrors_python() {
        // the exact semantics the gate's Path(a.file).match(pattern) has
        // (verified against Python's PurePath.match)
        assert!(pure_path_match("tools/a.py", "tools/**"));
        assert!(pure_path_match("a/tools/b.py", "tools/**")); // right-aligned
        assert!(!pure_path_match("tools/x/y.py", "tools/**"));
        assert!(!pure_path_match("tools", "tools/**"));
        assert!(pure_path_match("tests/test_x.py", "tests/**"));
        assert!(pure_path_match("app/tests/test_z.py", "tests/**")); // right-aligned
        assert!(!pure_path_match("tests/sub/test_y.py", "tests/**"));
        assert!(pure_path_match("x/y.py", "*.py"));
        assert!(!pure_path_match("z.txt", "*.py"));
    }

    #[test]
    fn group_expansion_matches_rule_groups() {
        let raw: toml::Value = toml::from_str("ignore = [\"group:test-discipline\"]").unwrap();
        let set = expand_ignores(&raw);
        assert!(set.contains("monkeypatch"));
        assert!(set.contains("skipif"));
        assert!(!set.contains("complexity"));
    }

    #[test]
    fn config_loads_global_and_per_path() {
        let dir = std::env::temp_dir().join(format!("cfg_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".lucidlint.toml"),
            "[lucidlint]\nignore = [\"vague-name\"]\n[lucidlint.\"tests/**\"]\nignore = [\"group:style\"]\n",
        )
        .unwrap();
        let cfg = load_lucidlint_config(&dir);
        assert!(cfg.is_ignored("vague-name", "anywhere.py"));
        assert!(cfg.is_ignored("magic-number", "tests/x.py")); // per-path group
        assert!(!cfg.is_ignored("magic-number", "app/x.py"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! The naming vocabulary both languages share: the vague role suffixes and
//! when a role name hides load-bearing code.

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

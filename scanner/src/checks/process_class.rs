//! `process-class`: a top-level class named for a process (Builder, Validator,
//! Importer) — the domain noun should name the thing, not the operation.

use ruff_python_ast::Stmt;

use crate::finding::Finding;
use crate::{stmt_line, ScanState};

/// The enumerated process set — a general `-er` test would fire on real
/// nouns, so the process words are enumerated. MINUS the GoF pattern
/// lexicon: those are the domain's own pattern names (Visitor, Iterator...).
pub const PROCESS_CLASS_WORDS: &[&str] = &[
    "Builder",
    "Importer",
    "Exporter",
    "Validator",
    "Converter",
    "Parser",
    "Reader",
    "Writer",
    "Fetcher",
    "Collector",
    "Handler",
    "Manager",
    "Provider",
    "Renderer",
    "Serializer",
    "Dispatcher",
    "Processor",
    "Analyzer",
    "Scheduler",
    "Runner",
    "Executor",
    "Authenticator",
];

pub const GOF_CLASS_WORDS: &[&str] = &[
    "Visitor", "Iterator", "Command", "Factory", "Strategy", "Proxy", "Adapter",
];

/// A top-level class named by a process noun — keep the name only when the
/// three-part test clears it; the GoF pattern names are the domain's own and
/// stay exempt.
pub fn process_class_findings(state: &mut ScanState, body: &[Stmt]) {
    for s in body {
        let Stmt::ClassDef(cls) = s else { continue };
        let name = cls.name.as_str();
        if GOF_CLASS_WORDS.iter().any(|w| name.ends_with(w)) {
            continue; // the domain's own pattern names are honest
        }
        if !PROCESS_CLASS_WORDS.iter().any(|w| name.ends_with(w)) {
            continue;
        }
        let line = stmt_line(state.source, s);
        state.findings.push(Finding { logical_start: None, seam_members: Vec::new(),
        col: 0,
        file: state.file.to_string(),
        line,
        function: name.to_string(),
        kind: "process-class".into(),
        severity: "fail".into(),
        message: format!(
            "class {name} is named for a process, not a thing. Keep the name only when all three hold: 1) the domain calls this component by that name; 2) the data it operates on is all already represented by well-designed classes; 3) data and logic still remain that legitimately fit better in a process class. If the data it operates on is still raw dicts, lists, or loose fields, represent it with classes first, then re-check 2 and 3."
        ), });
    }
}

// ------------------------------------------------------------- #32 closure-cluster

//! Fix-directive rendering: the tail a finding carries when the tool can name
//! the command that applies its fix, and the complexity message it routes.

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

/// The structured fix kind of a RAW finding message: the first token after
/// the `— fix: ` directive tail. None when the message carries no directive
/// (fix-less findings never construct a Fix — Sixth-pass BQ1).
pub fn fix_kind_of_message(message: &str) -> Option<&str> {
    let pos = message.rfind("— fix: ")?;
    let tail = &message[pos + "— fix: ".len()..];
    let kind = tail.split_whitespace().next()?;
    if kind == "lucidlint" {
        return None; // the rendered full-command template — not a directive
    }
    Some(kind)
}

/// The rule's default fix kind (the catalog's Rule.fix via KIND_FIX) — the
/// structured fix_kind for findings whose message carries no shape-routed
/// directive (large-function -> extract-method, partition etc.).
pub fn kind_fix_default(kind: &str) -> &'static str {
    for (k, fix) in crate::rules_gen::KIND_FIX {
        if *k == kind {
            return fix;
        }
    }
    ""
}

#!/usr/bin/env python3
"""Generate the derived rule artifacts from the ONE registration point:
`rule_metadata.CATALOG`.

Writes two files (the `make rules` step):

- RULES.md rule tables (between the RULES-GENERATED markers);
- `scanner/src/rules_gen.rs` — the Rust core's FAMILY_KINDS, STANDARD_KINDS,
  FAMILY_VARIANTS, `final_kind`, and `rule_groups`, so a rule registered in
  the catalog needs no Rust-side metadata edit (the review-log B6 lesson:
  the family/variant map drifted because it was a second registration
  point).

It validates as it goes:

- every kind the scanner can emit (the `kind: "k".into()` literals across
  the scanner source, plus the cc-path complexity emitter) has a catalog
  entry — a new family without one fails the generation;
- every catalog entry is an emitted kind — a dead row is removed;
- each entry's severity matches the scanner's emission where that is
  parseable.

Usage: `make rules` (writes both files) or `scripts/gen-rules.py --check`
(fails when either is stale — the gate runs this).
"""

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

import rule_metadata  # noqa: E402  # the sys.path bootstrap above precedes the import — inherent to the pattern

NON_LITERAL_KINDS = {"complexity"}

START = "<!-- RULES-GENERATED:START -->"
END = "<!-- RULES-GENERATED:END -->"
RULES_RS = ROOT / "scanner" / "src" / "rules_gen.rs"

# The three Finding-construction shapes the scanner uses — each captures
# the kind and its severity in one match:
_FINDING_PATTERNS: tuple = (
    # Finding struct: kind: "k".into(), ... severity: "s"
    re.compile(r'kind: "([a-z-]+)"\.into\(\),\s*severity: "(fail|warn)"'),
    # helper call: self.finding("k", "s", ...) / finding("k", "s", ...)
    re.compile(r'finding\(\s*"([a-z-]+)",\s*"(fail|warn)"'),
    # (message, "k") tuple handed to out.push(Finding { ... severity: "s" }
    # (the unused family's message variants share one construction; the
    # kind is a tuple element, the severity lives in the Finding — one
    # regex spans both, which is how the unused fail/warn drift surfaced)
    re.compile(r'"([a-z-]+)",\s*\)\s*\}\s*;\s*\n\s*out\.push\(Finding\s*\{[^}]*?severity: "(fail|warn)"'),
)


def scanner_sources() -> list[Path]:
    """The scanner's own source — rules_gen.rs is generated FROM the
    catalog, so scanning it for kinds would be circular."""
    return sorted(p for p in (ROOT / "scanner" / "src").glob("*.rs") if p.name != "rules_gen.rs")
def _prod_text(src: Path) -> str:
    """The file's production code: everything before the first `#[cfg(test)]`
    module. Test fixtures construct Findings with stand-in kinds (the LSP
    severity test uses \"except\") that are not real emissions. The split is
    anchored at line start — a doc-comment mention of the attribute is not a
    module boundary."""
    return re.split(r"(?m)^#\[cfg\(test\)\]", src.read_text(), maxsplit=1)[0]


def emitted_kinds() -> set[str]:
    """Every kind the scanner can emit: the kind/severity literals in its
    own source (any of the three construction shapes) plus the known
    non-literal emitters."""
    kinds = set(NON_LITERAL_KINDS)
    for src in scanner_sources():
        text = _prod_text(src)
        for pattern in _FINDING_PATTERNS:
            kinds.update(m.group(1) for m in pattern.finditer(text))
    return kinds


def emitted_severity(kind: str) -> str | None:
    """The scanner's severity for `kind`, from the kind/severity literal
    pairs. None when the kind emits through an indirect helper (unparseable
    — metadata is trusted)."""
    for src in scanner_sources():
        text = _prod_text(src)
        for pattern in _FINDING_PATTERNS:
            for m in pattern.finditer(text):
                if m.group(1) == kind:
                    return m.group(2)
    return None







def validate() -> list[str]:
    """Completeness + severity drift checks. Returns a list of problems."""
    problems: list[str] = []
    emitted = emitted_kinds()
    registered = set(rule_metadata.CATALOG.kinds())
    for kind in sorted(emitted - registered):
        problems.append(f"kind '{kind}' is emitted by the scanner but has no rule_metadata entry")
    for kind in sorted(registered - emitted):
        problems.append(f"kind '{kind}' has a rule_metadata entry but the scanner never emits it")
    for rule in rule_metadata.CATALOG.rules:
        emitted_sev = emitted_severity(rule.kind)
        if emitted_sev is not None and emitted_sev != rule.severity:
            problems.append(
                f"kind '{rule.kind}': metadata says {rule.severity}, scanner emits {emitted_sev}"
            )
        if rule.display_group not in rule_metadata.GROUP_INFO:
            problems.append(f"kind '{rule.kind}': unknown display group '{rule.display_group}'")
    return problems


def render_rules_md() -> str:
    """The marked region: every display group's section + table."""
    problems = validate()
    if problems:
        raise SystemExit("rule_metadata drift:\n  " + "\n  ".join(problems))
    out = [START]
    for group, (header, intro) in rule_metadata.GROUP_INFO.items():
        rows = [r for r in rule_metadata.CATALOG.rules if r.display_group == group]
        out.append("")
        out.append(f"## {header}")
        if intro:
            out.append("")
            out.append(intro)
        out.append("")
        out.append("| Rule | Severity | Language | What it checks |")
        out.append("|---|---|---|---|")
        for rule in rows:
            display = rule.display_name or rule.kind
            sev = f"**{rule.severity}**" if rule.severity == "warn" else rule.severity
            out.append(f"| **{display}** | {sev} | {rule.languages} | {rule.description} |")
        if group == "cross-cutting":
            out.append(
                "| **standard** | — | — | Catch-all for findings that don't "
                "fit the named families above. The finding's message "
                "explains what's wrong. |"
            )
    out.append("")
    out.append(END)
    return "\n".join(out)


def render_rules_rs() -> str:
    """The generated Rust metadata module — everything derived from the
    catalog; hand-editing it is a stale-file drift the gate catches."""
    rules = rule_metadata.CATALOG.rules
    kinds = ", ".join(f'"{r.kind}"' for r in rules)
    standard = ", ".join(f'"{k}"' for k in rule_metadata.CATALOG.standard_kinds())
    families = rule_metadata.CATALOG.families()
    groups = rule_metadata.CATALOG.groups()
    group_order = ["architecture", "style", "test-discipline", "suppression"]
    out = [
        "// GENERATED by scripts/gen-rules.py (`make rules`) — DO NOT EDIT.",
        "//",
        "// The single source of truth is rule_metadata.py's RuleCatalog:",
        "// adding a rule there and running `make rules` regenerates this",
        "// module, RULES.md, and the Python RULE_GROUPS map. The drift gate",
        "// (`make rules --check` / test_rules_md_is_generated) fails when",
        "// this file is stale — hand-editing it is a second registration",
        "// point and the one thing this module exists to forbid.",
        "",
        "/// Every kind the catalog registers — the scanner's contract.",
        f"pub const FAMILY_KINDS: &[&str] = &[{kinds}];",
        "",
        "/// Kinds that deliberately collapse to the \"standard\" display",
        "/// bucket — their messages carry the rule; a named `final_kind`",
        "/// bucket is optional for them.",
        f"pub const STANDARD_KINDS: &[&str] = &[{standard}];",
        "",
        "/// Family -> variant kinds, for family suppressions (`ignore",
        "/// latent-class <why>` covers every variant). Derived from the",
        "/// catalog's `display` buckets — a variant missing here is a",
        "/// registration drift, not a judgment call (review-log B6).",
        "pub const FAMILY_VARIANTS: &[(&str, &[&str])] = &[",
    ]
    for fam, members in families.items():
        out.append(f'    ("{fam}", &[{", ".join(f'"{m}"' for m in members)}]),')
    out.append("];")
    out.append("")
    out.append("/// kind -> display bucket — the lookup table final_kind scans.")
    out.append("/// A match of 50+ arms would be a complexity finding; data")
    out.append("/// stays flat (a linear scan over the entries is nothing).")
    out.append("pub const DISPLAY_BUCKETS: &[(&str, &str)] = &[")
    for r in rules:
        bucket = r.display or r.kind
        out.append(f'    ("{r.kind}", "{bucket}"),')
    out.append("];")
    out.append("")
    out.append("/// The display bucket for a finding kind — `final_kind` output.")
    out.append("pub fn final_kind(kind: &str) -> &'static str {")
    out.append("    for (k, bucket) in DISPLAY_BUCKETS {")
    out.append("        if *k == kind {")
    out.append("            return bucket;")
    out.append("        }")
    out.append("    }")
    out.append('    "standard"')
    out.append("}")
    out.append("")
    out.append("/// The config `group:` expansion — the LSP's mirror of the")
    out.append("/// gate's RULE_GROUPS (both derived from the same catalog).")
    out.append("pub fn rule_groups() -> &'static [(&'static str, &'static [&'static str])] {")
    out.append("    &[")
    for group in group_order:
        members = sorted(groups.get(group, set()))
        out.append(f'        ("{group}", &[{", ".join(f'"{m}"' for m in members)}]),')
    out.append("    ]")
    out.append("}")
    out.append("")
    return "\n".join(out)


def _fmt_rust(code: str) -> str:
    """Run the generated Rust through rustfmt so the committed file is
    fmt-clean and the --check comparison is stable (cargo fmt would
    otherwise rewrap the long const lines and make every generated file
    look stale). Falls back to the raw output when rustfmt is absent."""

    try:
        proc = subprocess.run(
            ["rustfmt", "--emit", "stdout"],
            input=code,
            capture_output=True,
            text=True,
            timeout=30,
        )
        if proc.returncode == 0 and proc.stdout:
            return proc.stdout
    # lucidlint: ignore swallow rustfmt absent — the raw output is the documented fallback
    except OSError:
        pass
    return code


def main() -> int:
    md_path = ROOT / "RULES.md"
    md_text = md_path.read_text()
    if START not in md_text or END not in md_text:
        print(f"error: {md_path} is missing the {START}/{END} markers")
        return 1
    md_region = render_rules_md()
    head, _mid, tail = md_text.partition(START)
    _mid, _marker, tail = tail.partition(END)
    new_md = head + md_region + tail
    new_rs = _fmt_rust(render_rules_rs() + "\n")
    if "--check" in sys.argv:
        problems = []
        if new_md != md_text:
            problems.append("RULES.md is stale — run `make rules` to regenerate")
        if new_rs != RULES_RS.read_text():
            problems.append(f"{RULES_RS} is stale — run `make rules` to regenerate")
        if problems:
            print("\n".join(problems))
            return 1
        print("RULES.md tables and rules_gen.rs are current")
        return 0
    md_path.write_text(new_md)
    RULES_RS.write_text(new_rs)
    print(f"regenerated {md_path} rule tables + {RULES_RS}")
    return 0



if __name__ == "__main__":
    sys.exit(main())

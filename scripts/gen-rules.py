#!/usr/bin/env python3
"""Generate the RULES.md rule tables from the canonical metadata.

The rule tables in RULES.md live between the markers
`<!-- RULES-GENERATED:START -->` and `<!-- RULES-GENERATED:END -->`; this
script rewrites only that region. It validates as it goes:

- every kind the scanner can emit (FAMILY_KINDS) has a metadata entry
  (a new family without one fails the generation — the table cannot
  silently go stale);
- every metadata entry is an emitted kind (a dead row is removed);
- each entry's severity matches the scanner's emission where that is
  parseable (kind + severity literals in one Finding construction).

Usage: `make rules` (writes RULES.md) or `scripts/gen-rules.py --check`
(fails when RULES.md is stale — the gate runs this).
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

import rule_metadata  # noqa: E402  # the sys.path bootstrap above precedes the import — inherent to the pattern

START = "<!-- RULES-GENERATED:START -->"
END = "<!-- RULES-GENERATED:END -->"


def family_kinds() -> set[str]:
    """Every kind the scanner can emit, parsed from the FAMILY_KINDS const."""
    text = (ROOT / "scanner" / "src" / "main.rs").read_text()
    start = text.index("pub const FAMILY_KINDS")
    end = text.index("];", start)
    return set(re.findall(r'"([a-z-]+)"', text[start:end]))


def emitted_severity(kind: str) -> str | None:
    """The scanner's severity for `kind`, from `kind: "k".into(), ...,
    severity: "s"` literals (the Finding struct order). None when the kind
    emits through an indirect helper (unparseable — metadata is trusted)."""
    for src in sorted((ROOT / "scanner" / "src").glob("*.rs")):
        text = src.read_text()
        m = re.search(
            rf'kind: "{re.escape(kind)}"\.into\(\),\s*severity: "(fail|warn)"', text
        )
        if m:
            return m.group(1)
    return None


def validate() -> list[str]:
    """Completeness + severity drift checks. Returns a list of problems."""
    problems: list[str] = []
    kinds = family_kinds()
    registered = {r[0] for r in rule_metadata.RULES}
    for kind in sorted(kinds - registered):
        problems.append(f"kind '{kind}' is emitted (FAMILY_KINDS) but has no rule_metadata entry")
    for kind in sorted(registered - kinds):
        problems.append(f"kind '{kind}' has a rule_metadata entry but the scanner never emits it")
    for kind, _name, _group, _langs, severity, _desc in rule_metadata.RULES:
        emitted = emitted_severity(kind)
        if emitted is not None and emitted != severity:
            problems.append(
                f"kind '{kind}': metadata says {severity}, scanner emits {emitted}"
            )
    return problems


def render() -> str:
    """The marked region: every display group's section + table."""
    problems = validate()
    if problems:
        raise SystemExit("rule_metadata drift:\n  " + "\n  ".join(problems))
    out = [START]
    for group, (header, intro) in rule_metadata.GROUP_INFO.items():
        rows = [r for r in rule_metadata.RULES if r[2] == group]
        out.append("")
        out.append(f"## {header}")
        if intro:
            out.append("")
            out.append(intro)
        out.append("")
        out.append("| Rule | Severity | Language | What it checks |")
        out.append("|---|---|---|---|")
        for kind, name, _g, langs, severity, desc in rows:
            display = name or kind
            sev = f"**{severity}**" if severity == "warn" else severity
            out.append(f"| **{display}** | {sev} | {langs} | {desc} |")
        if group == "cross-cutting":
            out.append(
                "| **standard** | — | — | Catch-all for findings that don't "
                "fit the named families above. The finding's message "
                "explains what's wrong. |"
            )
    out.append("")
    out.append(END)
    return "\n".join(out)


def main() -> int:
    path = ROOT / "RULES.md"
    text = path.read_text()
    if START not in text or END not in text:
        print(f"error: {path} is missing the {START}/{END} markers")
        return 1
    region = render()
    head, _mid, tail = text.partition(START)
    _mid, _marker, tail = tail.partition(END)
    new_text = head + region + tail
    if "--check" in sys.argv:
        if new_text != text:
            print("RULES.md is stale — run `make rules` to regenerate")
            return 1
        print("RULES.md tables are current")
        return 0
    path.write_text(new_text)
    print(f"regenerated {path} rule tables")
    return 0


if __name__ == "__main__":
    sys.exit(main())

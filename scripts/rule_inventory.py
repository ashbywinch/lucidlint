"""The scanner's emissions, read from its own source.

`gen-rules.py` needs to know which kinds the core can emit and at what severity,
so a rule registered in `rule_metadata.CATALOG` and a rule the core emits cannot
drift apart. The three Finding-construction shapes are matched here; a kind that
emits through an indirect helper is unparseable and the catalog is trusted for
it (`NON_LITERAL_KINDS`).
"""
# lucidlint: ignore-file global-state the scanner's construction patterns and
# non-literal kinds are the inventory's configuration — read once, never rebound

import re
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

import rule_metadata  # noqa: E402  # the sys.path bootstrap above precedes the import — inherent to the pattern

NON_LITERAL_KINDS = {
    # the scanner never emits these as a literal kind/severity pair: the
    # loop family's mutating/sequence variants all flow through
    # LoopFunctionCtx.push_finding (kind is a parameter, severities warn).
    # The severity string IS literal in the construction, but the regex
    # needs the pair in one match — metadata is trusted for these.
    "complexity",
    "mutating-loop",
    "loop-sequence",
    "loop-hoist",
    # churn-untested's severity is CONDITIONAL, not literal in the construction:
    # fail when the repo's coverage snapshot is fresh, warn ("verify") when the
    # snapshot predates the tests — R6 forbids a hard claim on stale data.
    "churn-untested",
}

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


class RuleInventory:
    """What the scanner can emit: every kind/severity literal in its source, and
    the agreement check against the catalog (the one registration point)."""

    def __init__(self, catalog: Any):
        self.catalog: Any = catalog

    @staticmethod
    def scanner_sources() -> list[Path]:
        """The scanner's own source — rules_gen.rs is generated FROM the
        catalog, so scanning it for kinds would be circular. Recursive: the
        families live in `src/` and in its concept submodules (`src/checks/`)."""
        return sorted(p for p in (ROOT / "scanner" / "src").rglob("*.rs") if p.name != "rules_gen.rs")

    @staticmethod
    def production_text(src: Path) -> str:
        """The file's production code: everything before the first `#[cfg(test)]`
        module. Test fixtures construct Findings with stand-in kinds (the LSP
        severity test uses \"except\") that are not real emissions. The split is
        anchored at line start — a doc-comment mention of the attribute is not a
        module boundary."""
        return re.split(r"(?m)^#\[cfg\(test\)\]", src.read_text(), maxsplit=1)[0]

    def emitted_kinds(self) -> set[str]:
        """Every kind the scanner can emit: the kind/severity literals in its
        own source (any of the three construction shapes) plus the known
        non-literal emitters."""
        kinds = set(NON_LITERAL_KINDS)
        for src in self.scanner_sources():
            text = self.production_text(src)
            for pattern in _FINDING_PATTERNS:
                kinds.update(m.group(1) for m in pattern.finditer(text))
        return kinds

    def emitted_severity(self, kind: str) -> str | None:
        """The scanner's severity for `kind`, from the kind/severity literal
        pairs. None when the kind emits through an indirect helper (unparseable
        — metadata is trusted)."""
        for src in self.scanner_sources():
            text = self.production_text(src)
            for pattern in _FINDING_PATTERNS:
                for m in pattern.finditer(text):
                    if m.group(1) == kind:
                        return m.group(2)
        return None

    def validate(self) -> list[str]:
        """Completeness + severity drift checks. Returns a list of problems."""
        problems: list[str] = []
        emitted = self.emitted_kinds()
        registered = set(self.catalog.kinds())
        for kind in sorted(emitted - registered):
            problems.append(f"kind '{kind}' is emitted by the scanner but has no rule_metadata entry")
        for kind in sorted(registered - emitted):
            problems.append(f"kind '{kind}' has a rule_metadata entry but the scanner never emits it")
        for rule in self.catalog.rules:
            emitted_sev = self.emitted_severity(rule.kind)
            if emitted_sev is not None and emitted_sev != rule.severity:
                problems.append(
                    f"kind '{rule.kind}': metadata says {rule.severity}, scanner emits {emitted_sev}"
                )
            if rule.section not in rule_metadata.GROUP_INFO:
                problems.append(f"kind '{rule.kind}': unknown section '{rule.section}'")
        # Sixth-pass BQ1 drift gate: a finding message advertises its fix as
        # `— fix: <kind>`; that kind MUST be a registered fix (FIX_REGISTRY) or
        # the stamp/notice/judgement machinery cannot classify it. Shape-routed
        # kinds (dispatch-registry, rule-table) are registered like any other.
        directive_kinds: set[str] = set()
        for src in self.scanner_sources():
            for m in re.finditer(r"— fix: (?!lucidlint )([a-z-]+)", self.production_text(src)):
                directive_kinds.add(m.group(1))
        for kind in sorted(directive_kinds - set(self.catalog.fixes.kinds())):
            problems.append(
                f"fix kind '{kind}' is advertised in a scanner message but not registered in FIX_REQUIRED_INPUTS"
            )

        return problems

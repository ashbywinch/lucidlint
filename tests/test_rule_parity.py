"""The parity corpus runner: one directory per rule/shape, discovered here.

The rule registry is the source of language coverage: a rule's
`implementations` say where its detector lives, and this runner requires a
fixture for every one of them (or a `not_applicable` reason). There is no
manifest of rules or shapes — the directory tree is the list.

Layout:

    tests/parity/<rule>/<shape>/python.py
    tests/parity/<rule>/<shape>/rust.rs
    tests/parity/<rule>/<shape>/expected.json
    tests/parity/<rule>/<shape>/rust.not_applicable   # with the reason

`expected.json` maps a present language to the findings that language's
fixture must produce, compared on the finding's function name.
"""

import json
import subprocess
from dataclasses import dataclass
from pathlib import Path

import pytest

import rule_metadata

ROOT = Path(__file__).resolve().parent.parent
PARITY = ROOT / "tests" / "parity"
BINARY = ROOT / "scanner" / "target" / "release" / "lucidlint"
LANGUAGE_FILES = {"python": "python.py", "rust": "rust.rs"}


def _corpus() -> list[tuple[str, str, Path]]:
    """Every (rule, shape, directory) the corpus holds, sorted for determinism."""
    return [
        (rule_dir.name, shape_dir.name, shape_dir)
        for rule_dir in sorted(path for path in PARITY.iterdir() if path.is_dir())
        for shape_dir in sorted(path for path in rule_dir.iterdir() if path.is_dir())
    ]


def _findings(rule: str, fixture: Path) -> list[dict]:
    """The rule's findings in one fixture, through the real scan core."""
    proc = subprocess.run(
        [str(BINARY), "--root", str(ROOT), "--include-tests", str(fixture.relative_to(ROOT))],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert proc.returncode == 0, proc.stderr
    contract = json.loads(proc.stdout)
    return sorted(
        (
            {"function": finding.get("function", "")}
            for finding in contract["findings"]
            if finding.get("signal") == rule
        ),
        key=lambda finding: finding["function"],
    )


def _expected(directory: Path) -> dict:
    path = directory / "expected.json"
    assert path.exists(), f"{directory}: missing expected.json"
    expected = json.loads(path.read_text())
    assert isinstance(expected, dict), f"{path}: expected a language -> findings object"
    return expected


def _language_fixtures(directory: Path, rule: rule_metadata.Rule) -> list[str]:
    """The languages with a real fixture; fails on a missing file or a bare
    `not_applicable` without a reason."""
    present: list[str] = []
    for language, file_name in LANGUAGE_FILES.items():
        fixture = directory / file_name
        if fixture.exists():
            assert language in rule.implementations, (
                f"{directory}: {file_name} exists but rule '{rule.kind}' implements {rule.implementations}"
            )
            present.append(language)
            continue
        if language not in rule.implementations:
            continue
        reason_file = directory / f"{language}.not_applicable"
        assert reason_file.exists(), (
            f"{directory}: rule '{rule.kind}' implements {language}, "
            f"so it needs {file_name} or {language}.not_applicable"
        )
        reason = reason_file.read_text().strip()
        assert reason, f"{reason_file}: not_applicable needs a reason"
    for other in directory.iterdir():
        if other.name in LANGUAGE_FILES.values():
            language = next(lang for lang, name in LANGUAGE_FILES.items() if name == other.name)
            assert language in rule.implementations, (
                f"{directory}: {other.name} exists but rule '{rule.kind}' does not implement {language}"
            )
    return present


@dataclass(frozen=True)
class Shape:
    """One validated shape directory: its rule, its present languages, and
    the findings each fixture must produce. Loading it is the validation, so
    a caller can never read `expected` before the rule check."""

    rule: rule_metadata.Rule
    directory: Path
    languages: tuple[str, ...]
    expected: dict[str, list[dict]]


def load_shape(rule_kind: str, directory: Path) -> Shape:
    rule = rule_metadata.CATALOG.by_kind(rule_kind)
    assert rule is not None, f"{directory}: '{rule_kind}' is not in the rule registry"
    languages = tuple(_language_fixtures(directory, rule))
    assert languages, f"{directory}: no fixture for any implemented language"
    expected = _expected(directory)
    assert set(expected) == set(languages), (
        f"{directory}: expected.json names {sorted(expected)} but the fixtures are {sorted(languages)}"
    )
    return Shape(rule=rule, directory=directory, languages=languages, expected=expected)


def test_parity_corpus_is_discovered_and_rules_exist():
    """The tree itself is the manifest: every rule directory names a catalog
    rule, and every shape loads (language files, reasons, expected findings)."""
    shapes = _corpus()
    assert shapes, "the parity corpus is empty"
    for rule_kind, _shape, directory in shapes:
        load_shape(rule_kind, directory)


@pytest.mark.parametrize(
    ("rule_kind", "shape", "directory"),
    _corpus(),
    ids=[f"{rule}-{shape}" for rule, shape, _directory in _corpus()],
)
def test_shape_parity(rule_kind: str, shape: str, directory: Path):
    """Both language fixtures express the shape the same way: the same rule
    findings on the same functions, or the same silence."""
    loaded = load_shape(rule_kind, directory)
    for language in loaded.languages:
        fixture = directory / LANGUAGE_FILES[language]
        assert _findings(rule_kind, fixture) == loaded.expected[language], (
            f"{fixture}: {rule_kind} findings disagree with expected.json"
        )


def _rule(**overrides):
    values = {
        "kind": "example-rule",
        "severity": "warn",
        "section": "style",
        "implementations": ("python",),
        "description": "an example rule",
    }
    values.update(overrides)
    return rule_metadata.Rule(**values)


def test_language_label_is_derived_from_implementations():
    """`Both` is not an input: it is what two code implementations mean. One
    language is that language; no implementation cannot be constructed."""
    assert _rule(implementations=("python",)).languages == "Python"
    assert _rule(implementations=("rust",)).languages == "Rust"
    assert _rule(implementations=("python", "rust")).languages == "Both"
    assert _rule(implementations=("graph",)).languages == "Graph"
    with pytest.raises(ValueError):
        _rule(implementations=())
    with pytest.raises(ValueError):
        _rule(implementations=("python", "go"))


def test_duplicate_kinds_cannot_be_registered():
    with pytest.raises(ValueError):
        rule_metadata.RuleCatalog([_rule(), _rule(description="the same kind again")])

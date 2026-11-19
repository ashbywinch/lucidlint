"""The project-wide lucidlint config: ignores (global and per-path) and the
per-signal guidance text appended to matching findings.

The config lets a team suppress entire rule groups or specific signals without
per-file suppression comments, and state house rules once. It is read from
`.lucidlint.toml` in the repo root, else `[tool.lucidlint]` in
`pyproject.toml`, and cached once per repo per process — the file cannot
change mid-run.
"""

from __future__ import annotations

import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, ClassVar

import rule_metadata

# the raw TOML payload — a wire-format blob, not a domain record (record-shape
# exempts the deserializer boundary by naming it)
_RawConfig = dict[str, Any]


@dataclass
class LucidlintConfig:
    """One repo's config, as the gate applies it: the globally ignored
    signals, the per-path ignores (glob, signals), and the house-rule
    guidance per signal."""

    global_ignore: set
    per_path_ignore: list
    guidance: dict[str, str] = field(default_factory=dict)

    # one read per repo per process: the file cannot change mid-run, and every
    # gate/scan call would otherwise re-parse it
    _CACHE: ClassVar[dict[Path, LucidlintConfig]] = {}

    @classmethod
    def load(cls, repo: Path) -> LucidlintConfig:
        """The repo's config, or empty defaults when it has none."""
        cached = cls._CACHE.get(repo)
        if cached is not None:
            return cached
        result = cls(set(), [])
        toml_path = repo / ".lucidlint.toml"
        if toml_path.is_file():
            with open(toml_path, "rb") as f:
                result._merge(tomllib.load(f).get("lucidlint", {}))
        else:
            pyproject = repo / "pyproject.toml"
            if pyproject.is_file():
                with open(pyproject, "rb") as f:
                    data = tomllib.load(f)
                tool_config = data.get("tool", {}).get("lucidlint", {})
                if tool_config:
                    result._merge(tool_config)
        cls._CACHE[repo] = result
        return result

    def _merge(self, raw: _RawConfig) -> None:
        """Fold one TOML table into this config."""
        groups = rule_metadata.CATALOG.groups()
        ignores = raw.get("ignore", raw.get("ignored_signals", []))
        if ignores is None:  # a config key present with no value iterates as empty
            ignores = []
        for item in ignores:
            item = item.strip()
            if item.startswith("group:"):
                signals = groups.kinds_in(item[6:])
                if signals:
                    self.global_ignore.update(signals)
            else:
                self.global_ignore.add(item)
        # per-path overrides: keys that are glob patterns
        for key, val in raw.items():
            if key in ("ignore", "ignored_signals"):
                continue
            if isinstance(val, dict) and "ignore" in val:
                path_ignores = set()
                for item in val["ignore"]:
                    item = item.strip()
                    if item.startswith("group:"):
                        signals = groups.kinds_in(item[6:])
                        if signals:
                            path_ignores.update(signals)
                    else:
                        path_ignores.add(item)
                self.per_path_ignore.append((key, path_ignores))
        # Guidance: signal -> house-rule text for matching findings. Keys
        # naming no known signal are dropped — a typo'd key would otherwise
        # silently do nothing while reading like it works.
        guidance = raw.get("guidance")
        if isinstance(guidance, dict):
            for sig, text in guidance.items():
                if isinstance(text, str) and sig in rule_metadata.CATALOG.kinds():
                    self.guidance[sig] = text.strip()

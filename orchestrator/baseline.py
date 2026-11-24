"""The acknowledged-action baseline: the file, the keys it holds, and what
matching them means.

A file that holds nothing — absent, blank, `[]`, `{}` — is an empty ledger:
the fresh-repo state, and never a warning. A file that holds something the
gate cannot read as a ledger (invalid JSON, another JSON type, a key or a
count it cannot use) is a configuration error, not an empty ledger: reading
it as empty would report acknowledged debt as new, or pass, while comparing
nothing (#127).

Exact-identity matching, the pyrefly pattern (Phase 5): an acknowledged key
(kind:file:line:function) that matches NO current finding is stale — drifted,
fixed, and deleted are the same case; no reconciliation is attempted.
`--update-baseline` regenerates the keys, as it does for pyrefly and
mypy-baseline. Pre-round 'actions' entries are counted and dropped, so the
caller prints ONE migration line instead of reading the file as empty debt
(#26-#34 B3).
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, NamedTuple

from orchestrator.action import Action


class _BaselineState(NamedTuple):
    """How much of a baseline file is current-format debt, and how much is
    pre-round legacy that cannot map to a key."""

    current: int
    legacy: int


class Baseline:
    """The acknowledged actions of one run's baseline file: the keys, the
    counts, and the flaw when the file is not a baseline at all."""

    # a baseline key is kind:file:line:function — four fields, the third numeric
    _KEY_PARTS = 4
    _KEY_LINE_INDEX = 2

    # the only keys a baseline carries
    _LEDGER_KEYS = ("actions", "config_ignored")

    # pre-round files stored other shapes — line-less keys, action dicts —
    # which cannot map to a current key identity
    KEY_PARTS = 4

    def __init__(self, path: Path | None = None) -> None:
        self.path: Path | None = path
        self.keys: set[str] = set()
        self.ignored: dict[str, int] = {}
        self.legacy: int = 0
        self.counts_recorded: bool = False
        self.problem: str = ""
        self._read()

    def _read(self) -> None:
        if self.path is None or not self.path.exists():
            return
        try:
            text = self.path.read_text()
        except (OSError, UnicodeDecodeError) as e:
            self.problem = self._problem(f"cannot be read ({e})")
            return
        if not text.strip():
            return
        try:
            data = json.loads(text)
        except json.JSONDecodeError as e:
            self.problem = self._problem(f"is not JSON ({e})")
            return
        flaw = self._read_ledger(data)
        if flaw:
            self.problem = self._problem(flaw)

    def _read_ledger(self, data: Any) -> str:
        """Fill the keys, counts and record flag from a parsed file, and
        return ""; or fill nothing and return the flaw that stops the gate
        comparing (content it would otherwise silently drop)."""
        if isinstance(data, list) and not data:
            return ""
        if not isinstance(data, dict):
            return Baseline._not_an_object(data)
        unknown = [key for key in data if key not in Baseline._LEDGER_KEYS]
        if unknown:
            return (
                f"holds the key '{unknown[0]}', which the gate does not read "
                "(a baseline holds 'actions' and 'config_ignored')"
            )
        entries = data.get("actions", [])
        if not isinstance(entries, list):
            return f"has 'actions' as a JSON {Baseline._json_type(entries)}, not a list of keys"
        flaw = self._read_counts(data.get("config_ignored", {}))
        if flaw:
            return flaw
        self.counts_recorded = "config_ignored" in data
        self.keys = {e for e in entries if self.entry_is_current(e)}
        self.legacy = sum(1 for e in entries if not self.entry_is_current(e))
        return ""

    def _read_counts(self, ignored: Any) -> str:
        """Fill the config-ignore counts, or return the flaw when the value is
        not a map of counts."""
        if not isinstance(ignored, dict):
            return (
                f"has 'config_ignored' as a JSON {Baseline._json_type(ignored)}, "
                "not a map of signal counts"
            )
        counts: dict[str, int] = {}
        for signal, count in ignored.items():
            if not isinstance(count, int) or isinstance(count, bool):
                return f"has 'config_ignored.{signal}' as {Baseline._quoted(count)}, not a count"
            counts[signal] = count
        self.ignored = counts
        return ""

    @staticmethod
    def _not_an_object(data: Any) -> str:
        """The flaw for a file whose parsed value is not a ledger object."""
        if isinstance(data, list):
            return f"holds a JSON list of {len(data)} entries, not an object with an 'actions' list"
        return f"holds a JSON {Baseline._json_type(data)}, not an object with an 'actions' list"

    def _problem(self, flaw: str) -> str:
        """The configuration error for a file that is not a baseline: what it
        holds, why ignoring it is worse than stopping, and the repair."""
        return (
            f"baseline {self.path} {flaw}. "
            "Ignoring the file would let the gate pass while comparing nothing. "
            f"Run --update-baseline --baseline {self.path} to rewrite it."
        )

    @staticmethod
    def _json_type(value: Any) -> str:
        """The JSON name for a parsed value, so the message says what the file
        holds rather than what Python calls it."""
        if value is None:
            return "null"
        if isinstance(value, bool):
            return "boolean"
        if isinstance(value, (int, float)):
            return "number"
        if isinstance(value, str):
            return "string"
        if isinstance(value, dict):
            return "object"
        return "list"

    @staticmethod
    def _quoted(value: Any) -> str:
        """How the message names a count's value: a string in quotes, any
        other JSON value by its type."""
        if isinstance(value, str):
            return f"'{value}'"
        return f"a JSON {Baseline._json_type(value)}"

    @staticmethod
    def entry_is_current(entry: Any) -> bool:
        """True when a baseline 'actions' entry is the current key shape
        (kind:file:line:function with a numeric line)."""
        if not isinstance(entry, str):
            return False
        parts = entry.split(":", Baseline._KEY_PARTS - 1)
        return len(parts) == Baseline._KEY_PARTS and parts[Baseline._KEY_LINE_INDEX].isdigit()

    @staticmethod
    def file_state(path: Path | None) -> _BaselineState:
        """How much of a baseline file is current debt; zero when absent or
        unreadable. Used to recognize a repo's own lucidlint.json when no
        --baseline was passed (#26-#34 B3)."""
        if not path or not path.exists():
            return _BaselineState(current=0, legacy=0)
        try:
            entries = json.loads(path.read_text()).get("actions", [])
        except (json.JSONDecodeError, AttributeError, TypeError, ValueError):
            return _BaselineState(current=0, legacy=0)
        if not isinstance(entries, list):
            return _BaselineState(current=0, legacy=0)
        return _BaselineState(
            current=sum(1 for e in entries if Baseline.entry_is_current(e)),
            legacy=sum(1 for e in entries if not Baseline.entry_is_current(e)),
        )

    @staticmethod
    def apply(unique: list[Action], baseline_keys: set[str]) -> list[str]:
        """Mark acknowledged actions so they report but never fail the gate.

        Returns the matchless keys; the count renders in the ledger so the
        reader sees why acknowledged debt re-reports.
        """
        current_keys = {a.key() for a in unique if a.severity != "warn"}
        matched = current_keys & baseline_keys
        for a in unique:
            if a.key() in matched:
                a.severity = "ack"
        return sorted(baseline_keys - current_keys)

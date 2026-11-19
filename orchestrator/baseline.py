"""The acknowledged-action baseline: the file, the keys it holds, and what
matching them means.

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
from orchestrator.console import CONSOLE


class _BaselineState(NamedTuple):
    """How much of a baseline file is current-format debt, and how much is
    pre-round legacy that cannot map to a key."""

    current: int
    legacy: int


class Baseline:
    """The acknowledged actions of one run's baseline file (best-effort: an
    absent or unreadable file acknowledges nothing)."""

    # pre-round files stored other shapes — line-less keys, action dicts —
    # which cannot map to a current key identity
    KEY_PARTS = 4

    def __init__(self, path: Path | None = None) -> None:
        self.path = path
        self.keys: set[str] = set()
        self.ignored: dict[str, int] = {}
        self.legacy = 0
        self._read()

    def _read(self) -> None:
        if self.path is None or not self.path.exists():
            return
        try:
            data = json.loads(self.path.read_text())
            entries = data.get("actions", [])
            entries = entries if isinstance(entries, list) else []
            self.keys = {e for e in entries if self.entry_is_current(e)}
            self.legacy = sum(1 for e in entries if not self.entry_is_current(e))
            ignored = dict(data.get("config_ignored", {}))
            self.ignored = {k: int(v) for k, v in ignored.items()}
        # lucidlint: ignore swallow corrupt baseline; gate unbaselined
        except (json.JSONDecodeError, AttributeError, TypeError, ValueError):
            CONSOLE.warn(f"baseline {self.path} unreadable — ignoring")

    @staticmethod
    def entry_is_current(entry: Any) -> bool:
        """True when a baseline 'actions' entry is the current key shape
        (kind:file:line:function with a numeric line)."""
        if not isinstance(entry, str):
            return False
        parts = entry.split(":", 3)
        return len(parts) == 4 and parts[2].isdigit()

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

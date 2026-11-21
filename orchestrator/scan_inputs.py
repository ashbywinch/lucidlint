"""The per-process directory the scan inputs are written into.

The JSON inputs one scan hands the binary (graph contract, churn, coverage)
are big — a graph dump is tens of MB — and the binary needs a PATH, not a
pipe. One stdlib TemporaryDirectory per process owns them: every input is a
file inside it, the directory removes itself (and anything left in it) at
exit, and a per-run input is deleted as soon as its scan has read it. There is
no registry to keep and no lock to take — the filesystem is the only shared
state, and each input's name is unique.
"""

from __future__ import annotations

import os
import tempfile
from pathlib import Path


class ScanInputs:
    """The one input directory this process writes into, created on first use
    so importing the module writes nothing."""

    def __init__(self) -> None:
        self._directory: tempfile.TemporaryDirectory[str] | None = None

    def directory(self) -> Path:
        """The process's scan-input directory."""
        if self._directory is None:
            self._directory = tempfile.TemporaryDirectory(prefix="lucidlint-inputs-")
        return Path(self._directory.name)

    def write(self, prefix: str, payload: str) -> Path:
        """Write one scan input for the binary inside the process directory."""
        fd, name = tempfile.mkstemp(prefix=prefix, suffix=".json", dir=self.directory())
        os.close(fd)  # mkstemp opens the file; the write below reopens it
        path = Path(name)
        path.write_text(payload, encoding="utf-8")
        return path

    @staticmethod
    def release(path: Path | None) -> None:
        """Delete a per-run input as soon as the scan that needed it has
        finished (churn, coverage). The graph contract is cached per repo for
        the process and is left for the directory's own cleanup."""
        if path is not None:
            path.unlink(missing_ok=True)


INPUTS = ScanInputs()

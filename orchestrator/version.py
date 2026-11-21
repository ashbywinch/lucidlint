"""The artifact's own version stamp.

A release bundle ships `version.txt` (the tag, written with a leading v) next
to the entry module; an installed package answers through its metadata; a
source checkout answers from `pyproject.toml`. The artifact's own stamp wins:
a bundle run inside an environment that has some other lucidlint installed
still reports its own tag. Never a hardcoded literal — that is the drift class
the README/version tests exist to catch.
"""

from __future__ import annotations

import tomllib
from importlib import metadata
from pathlib import Path

from orchestrator.artifact import artifact_root


class Version:
    """The version of one artifact, from the directory its entry module lives
    in (that directory is where a bundle's version.txt and a checkout's
    pyproject.toml sit)."""

    def __init__(self, here: Path | None = None) -> None:
        # the artifact root: the entry module sits beside this package, so the
        # package's parent is the bundle root, the checkout root, or the
        # site-packages that holds the distribution
        self.here = here or artifact_root()

    def from_bundle(self) -> str | None:
        """The version.txt a release bundle ships (the tag, written with a
        leading v) — None when the file is absent or empty. The bundle has no
        pyproject.toml, so without this the bundle's Python entry point
        reports 0.0.0.dev while bin/lucidlint reports the tag."""
        try:
            raw = (self.here / "version.txt").read_text().strip().lstrip("v")
        except OSError:
            return None
        return raw or None

    def detect(self) -> str:
        """The artifact's stamp: the bundle's version.txt, else the installed
        package version, else the checkout's pyproject.toml (the single source
        for a checkout; the release workflow writes version.txt from the tag)."""
        bundled = self.from_bundle()
        if bundled is not None:
            return bundled
        try:
            return metadata.version("lucidlint")
        except Exception:
            try:
                with (self.here / "pyproject.toml").open("rb") as f:
                    return str(tomllib.load(f)["project"]["version"])
            except Exception:
                return "0.0.0.dev"


VERSION = Version().detect()

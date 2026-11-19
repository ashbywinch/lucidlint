"""The host environment every orchestrator module runs in.

The seams are the entry module's bindings — `lucidlint.subprocess`,
`lucidlint._pygit2`, `lucidlint._GraphStore`, `lucidlint._Registry` — because
that is the public surface a consumer or the test suite substitutes
(`ch.subprocess = fake`, `ch._pygit2 = None`). A module that needs one asks the
host for its CURRENT binding at call time, so the substitution reaches the call
wherever it is made.

"""

from __future__ import annotations

from pathlib import Path

from orchestrator.substitutable import Substitutable


class Host:
    """The environment of one run: the process runner, the optional
    git/graph libraries, and the location of the vendored deps directory.
    Each seam is a proxy over the entry module's binding, so one substitution
    there replaces the collaborator for the whole run."""

    @property
    def artifact_root(self) -> Path:
        """Where this artifact lives: the entry module sits beside the package,
        so the package's parent is the bundle root, the checkout root, or the
        site-packages that holds the distribution — the one home of `deps/`,
        `version.txt`, `bin/` and `lucidlint_bin/`."""
        return Path(__file__).resolve().parent.parent

    def __init__(self) -> None:
        self.subprocess = Substitutable("subprocess")
        self.pygit2 = Substitutable("_pygit2")
        self.graph_store = Substitutable("_GraphStore")
        self.graph_registry = Substitutable("_Registry")


HOST = Host()

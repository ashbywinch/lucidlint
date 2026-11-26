"""The host environment every orchestrator module runs in.

One run's collaborators: the process module, the optional git and graph
libraries, and the optional fix engine. A module receives the host through its
constructor and reads the real objects from it, so a test constructs a host
with its fakes and every call in that run sees them.

`Host.default()` is the single place that imports the concrete libraries: the
composition root (`Cli`) builds one, and an absent optional library is `None`
(a supported mode), never an import failure.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from importlib import import_module
from typing import Any


def _optional(name: str) -> Any:
    """The module, or None when this environment does not have it."""
    try:
        return import_module(name)
    except ImportError:
        return None


_UNLOADED = object()


@dataclass(frozen=True)
class Host:
    """One run's environment: the process runner, the optional git/graph
    libraries, and the optional fix engine."""

    process: Any
    pygit2: Any
    graph_store: Any
    graph_registry: Any
    fix_engine: Any = _UNLOADED
    # The lazily imported engine lives in a mutable holder: the frozen
    # Host's fields (and hash) never change after construction.
    _loaded: dict[str, Any] = field(default_factory=dict, repr=False, compare=False)

    @classmethod
    def default(cls) -> Host:
        """The real environment: stdlib subprocess plus whichever optional
        libraries this interpreter can import. The graph extra ships its store
        and registry together, so one absent leaves the pair absent."""
        graph = _optional("code_review_graph.graph")
        registry = _optional("code_review_graph.registry")
        graph_available = graph is not None and registry is not None
        return cls(
            process=import_module("subprocess"),
            pygit2=_optional("pygit2"),
            graph_store=graph.GraphStore if graph_available else None,
            graph_registry=registry.Registry if graph_available else None,
        )

    def load_fix_engine(self) -> Any:
        """The fix engine: an explicitly injected one, or the real module
        imported once and cached in `_loaded`. A scan never pays for libcst,
        which only the `fix` command needs."""
        # an injected engine (tests, a custom composition) is used as given
        if self.fix_engine is not _UNLOADED:
            return self.fix_engine
        if "fix_engine" not in self._loaded:
            self._loaded["fix_engine"] = _optional("fix_engine")
        return self._loaded["fix_engine"]

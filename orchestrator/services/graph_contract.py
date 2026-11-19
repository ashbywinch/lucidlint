"""The code-review-graph export contract, built through the tool's own public
API (GraphStore + Registry).

The gate never touches the SQLite schema or the DB location, and never shells
out: it asks the graph tool for its store, walks the store's public reads, and
writes one JSON document the Rust binary consumes via `--graph`. None = tool
missing or no graph for this repo.
"""

from __future__ import annotations

import json
import sqlite3
from dataclasses import asdict, dataclass
from pathlib import Path

from orchestrator.console import CONSOLE
from orchestrator.host import HOST
from orchestrator.rust_scan import RustScan
from orchestrator.scan_inputs import INPUTS


@dataclass(frozen=True)
class _GraphNode:
    """One node of the graph tool's export (its public read, verbatim)."""

    kind: str
    name: str
    qualified_name: str
    file_path: str
    line_start: int
    line_end: int
    params: object
    return_type: object
    community_id: object


@dataclass(frozen=True)
class _GraphEdge:
    """One edge of the graph tool's export."""

    kind: str
    source: str
    target: str
    file_path: str


@dataclass(frozen=True)
class _GraphExport:
    """The contract document: the version, the nodes, the edges, and the
    community names. The Rust core consumes exactly this shape."""

    contract_version: int
    nodes: list[_GraphNode]
    edges: list[_GraphEdge]
    communities: dict[str, str]


class GraphContract:
    """The graph tool's export for one repo, cached per repo for the process."""

    CONTRACT_VERSION = 1

    def __init__(self) -> None:
        self._cache: dict[Path, Path | None] = {}

    def contract(self, repo: Path) -> Path | None:
        if repo in self._cache:
            return self._cache[repo]
        if HOST.graph_store.get() is None:
            self._cache[repo] = None
            return None
        result: Path | None = None
        try:
            registry = HOST.graph_registry
            data_dir = registry().get_data_dir_for_repo(str(repo)) if registry.get() else None
            db = Path(data_dir) / "graph.db" if data_dir else repo / ".code-review-graph" / "graph.db"
            if not db.exists():
                self._cache[repo] = None
                return None
            store = HOST.graph_store(db)
            with store:
                community_ids = store.get_all_community_ids()
                nodes = []
                for file_path in store.get_all_files():
                    for gnode in store.get_nodes_by_file(file_path):
                        nodes.append(
                            _GraphNode(
                                kind=gnode.kind,
                                name=gnode.name,
                                qualified_name=gnode.qualified_name,
                                file_path=gnode.file_path,
                                line_start=gnode.line_start,
                                line_end=gnode.line_end,
                                params=gnode.params,
                                return_type=gnode.return_type,
                                community_id=community_ids.get(gnode.qualified_name),
                            )
                        )
                edges = []
                for e in store.get_all_edges():
                    edges.append(
                        _GraphEdge(
                            kind=e.kind,
                            source=e.source_qualified,
                            target=e.target_qualified,
                            file_path=e.file_path,
                        )
                    )
                communities = {}
                for row in store.get_communities_list():
                    communities[str(row["id"])] = row["name"]
            export = _GraphExport(
                contract_version=GraphContract.CONTRACT_VERSION,
                nodes=nodes,
                edges=edges,
                communities=communities,
            )
            result = INPUTS.write("lucidlint-graph-", json.dumps(asdict(export), separators=(",", ":")))
        # The direct code-review-graph API can raise sqlite3.Error
        # (corrupt/older graph.db), KeyError (rows without the expected
        # columns), or TypeError/AttributeError (version mismatch) — all must
        # degrade, not crash the scan (PRD R21). The scanner failure modes ride
        # along: the export's only other work is a subprocess-free temp write.
        except (*RustScan.SCANNER_FAILURES, sqlite3.Error, KeyError, TypeError, AttributeError) as e:
            CONSOLE.warn(f"graph contract export failed for {repo}: {e}")
            result = None
        self._cache[repo] = result
        return result

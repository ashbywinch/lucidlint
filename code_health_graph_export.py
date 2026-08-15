"""Graph contract export for the code-health gate.

The gate's graph families (hub-file, high-risk, large-function, cycles,
layer/folder-mix) used to query the code-review-graph SQLite schema
directly at a hardcoded location — both wrong: the tool version-migrates
its own schema, and `--data-dir` relocates the DB. This adapter instead
opens the graph through the tool's OWN public API (GraphStore + the
registry's data-dir resolution) and emits a versioned, schema-neutral
JSON contract. The Rust scan core consumes it via `--graph <file>`; the
gate never writes SQL and never assumes where the DB lives.

    code_health_graph_export.py --repo <root>   # contract JSON on stdout

Exit codes: 0 = contract emitted, 2 = graph tool missing or no graph.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

CONTRACT_VERSION = 1

try:
    from code_review_graph.graph import GraphStore
    from code_review_graph.registry import Registry
except ImportError:  # code-health: ignore except the graph tool is optional — the gate
    # degrades to the non-graph families with a clear message, never a crash
    GraphStore = None  # type: ignore[assignment] # absent when the tool is missing
    Registry = None  # type: ignore[assignment] # absent when the tool is missing


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--repo", required=True, help="repository root")
    args = ap.parse_args()
    repo = Path(args.repo).resolve()
    if GraphStore is None or Registry is None:
        print("code-review-graph tool not installed — `uv tool install code-review-graph`", file=sys.stderr)
        return 2
    data_dir = Registry().get_data_dir_for_repo(str(repo))
    db = Path(data_dir) / "graph.db" if data_dir else repo / ".code-review-graph" / "graph.db"
    if not db.exists():
        print(f"no graph at {db} — run `code-review-graph build --repo {repo}` first", file=sys.stderr)
        return 2
    store = GraphStore(db)
    with store:
        nodes = []
        community_ids = store.get_all_community_ids()
        for file_path in store.get_all_files():
            for gnode in store.get_nodes_by_file(file_path):
                nodes.append(
                    {
                        "kind": gnode.kind,
                        "name": gnode.name,
                        "qualified_name": gnode.qualified_name,
                        "file_path": gnode.file_path,
                        "line_start": gnode.line_start,
                        "line_end": gnode.line_end,
                        "params": gnode.params,
                        "return_type": gnode.return_type,
                        "community_id": community_ids.get(gnode.qualified_name),
                    }
                )
        edges = []
        for e in store.get_all_edges():
            edges.append(
                {
                    "kind": e.kind,
                    "source": e.source_qualified,
                    "target": e.target_qualified,
                    "file_path": e.file_path,
                }
            )
        communities = {}
        for row in store.get_communities_list():
            communities[str(row["id"])] = row["name"]
    # code-health: ignore record-shape wire-format envelope — a class is ceremony for JSON
    contract = {
        "contract_version": CONTRACT_VERSION,
        "nodes": nodes,
        "edges": edges,
        "communities": communities,
    }
    json.dump(contract, sys.stdout, separators=(",", ":"))
    print()
    return 0


if __name__ == "__main__":
    sys.exit(main())

"""The gate's two external services in one object: the Rust scan core and the
graph contract exporter. Both cache per repo, so a run holds one of each."""

from __future__ import annotations

from orchestrator.rust_scan import RustScan
from orchestrator.services.graph_contract import GraphContract


class Services:
    """The one pair of collaborating services a run needs, wired once."""

    def __init__(self) -> None:
        self.graph = GraphContract()
        self.rust_scan = RustScan(self.graph)


SERVICES = Services()

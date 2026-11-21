#!/usr/bin/env python3
"""lucidlint.py — the deterministic lucidlint gate: a thin orchestrator
over the Rust scan core.

The finding engine is the Rust binary (scanner/): every family — per-file,
latent-class partition, the test-only rules, duplicate/unused, record-shape,
complexity, the graph families (via the versioned export contract),
hotspot, over-abstraction, docs — computes there. This orchestrator gathers
the inputs (git file list/history, the graph contract, churn, coverage),
converts the contract findings to Actions verbatim, and renders the gate:
baselines, dedupe/merge, priority, diff marking, exit codes.

    python3 lucidlint.py --repo /path/to/repo --json
    echo $?   # 1 when there is work to do

The scan thresholds live in the binary (schema 4): CC>=15, fn>=120 lines,
file>=150 edges, risk>=0.8, hotspot top 10% by churn with CC>=15.
Philosophy: the metrics are *proxies* for code that is obviously correct
and cheap to change; each message says what to do in those terms.
"""

from __future__ import annotations

import os
import sys

from orchestrator.cli import Cli
from orchestrator.host import Host
from orchestrator.vendoring import vendor_path

__all__ = ["main"]

# the entry module's own directory: the release bundle's root (deps/,
# version.txt) sits beside it
_here = os.path.dirname(os.path.abspath(__file__))


def main(host: Host | None = None) -> int:
    """The process entry (`lucidlint:main`): the CLI does the work.

    `host` is parameterisation, not a shim: an embedding client or a test
    passes its own collaborators; the console script passes none, and the
    default host imports the real ones.
    """
    # The release bundle self-contains the fix engine's libcst in a sibling
    # `deps/` dir: when one sits next to this file (the bundle, not a wheel
    # install), prefer it so the fix path works with no pip step.
    vendor = vendor_path(_here)
    if vendor is not None and vendor not in sys.path:
        sys.path.insert(0, vendor)
    return Cli(Cli.parse_args(), host=host).run()


if __name__ == "__main__":
    sys.exit(main())

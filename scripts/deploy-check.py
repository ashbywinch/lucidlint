#!/usr/bin/env python3
"""The deployment check (scripts/ is dev tooling, not shipped).

Generates a mini project with deliberate fixable findings, scans it with an
INSTALLED lucidlint (a freshly built wheel in a clean venv, or the release
bundle's `python3 lucidlint.py`), applies every fix directive the report
carries, and re-scans to GATE: PASS.

This is the packaging smoke test: each step depends on a different part of
the package, so a packaging error fails loudly instead of silently passing:
- the scan step needs the Rust binary (embedded wheel package-data; the
  sibling `bin/lucidlint` next to the bundle's lucidlint.py);
- the fix step needs fix_engine.py and the libcst dependency (a wheel
  declares it in [project] dependencies, the release bundle vendors it in
  `deps/` — a missing one breaks this import);
- the verdict and fix subcommands need the entry point and every shipped
  module (the report footer imports rule_metadata.py).

Usage: deploy-check.py --lucidlint <command-prefix> --project <out-dir>

--lucidlint is the shlex-split command that RUNS lucidlint: an installed
binary path (`.venv/bin/lucidlint`, `venv/Scripts/lucidlint.exe`) or a
prefix like `python3 dist/lucidlint-dev-x86_64/bin/lucidlint.py` for the
release bundle (which has no console script).
"""

from __future__ import annotations

import argparse
import json
import re
import shlex
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

# The mini project: one fixable finding per file, every function reachable
# from main() (reachable functions avoid the unused family — the check
# asserts the finding kinds it expects, not incidental noise).
PROJECT_FILES = {
    "a.py": (
        "# lucidlint: ignore magic-number nothing on this line\n"
        "\n"
        "def a():\n"
        "    return 1 + 1\n"
    ),
    "b.py": (
        "def unreach():\n"
        "    return 1\n"
        "    x = 2\n"
    ),
    "c.py": (
        "def noop():\n"
        "    x = 1\n"
        "    x + 1\n"
        "    return x\n"
    ),
    "d.py": (
        "def callee(a, b):\n"
        "    return a + b\n"
        "\n"
        "def caller():\n"
        "    return callee(10, 20)\n"
    ),
    "main.py": (
        "def main():\n"
        "    a()\n"
        "    unreach()\n"
        "    noop()\n"
        "    caller()\n"
        "\n"
        'if __name__ == "__main__":\n'
        "    main()\n"
    ),
}

# The fail-tier findings the project must produce — the gate's exit 1 on
# them is the assertion that the scan actually ran.
EXPECTED_FAIL_KINDS = ("stale-suppression", "unreachable", "noop-statement")

# magic-number's directive names the constant placeholder (`--name <CONST>`) —
# the check supplies the name, exactly as an agent would.
CONST_NAME = "MAX_RETRIES"


@dataclass(frozen=True)
class Action:
    """One finding from the scan JSON contract — the fields the check uses."""

    kind: str
    severity: str
    file: str
    line: int
    message: str


def run(cmd: list[str], cwd: Path) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(cmd, capture_output=True, text=True, cwd=cwd)
    except OSError as exc:
        raise SystemExit(f"deploy-check: cannot run {cmd[0]}: {exc}") from exc


def parse_actions(stdout: str) -> list[Action] | None:
    try:
        raw = json.loads(stdout)["actions"]
    except (ValueError, KeyError, TypeError):
        return None
    return [
        Action(
            kind=a["kind"],
            severity=a["severity"],
            file=a["file"],
            line=a["line"],
            message=a["message"],
        )
        for a in raw
    ]


def fail(msg: str) -> int:
    print(f"deploy-check: {msg}", file=sys.stderr)
    return 1


class _DeployCheck:
    """The gate steps: run the scanner on the project, assert the expected
    fail findings, apply the fix directives, verify the re-scan is clean.
    The (lucidlint command, project) pair travels together — a class holds it."""

    def __init__(self, cmd: list[str], project: Path):
        self.cmd: list[str] = cmd
        self.project: Path = project

    def find_step(self) -> int:
        scan = run(self.cmd + ["--repo", str(self.project), "--json"], cwd=self.project)
        actions = parse_actions(scan.stdout)
        if actions is None:
            # the command itself failed (bad path, missing interpreter,
            # traceback) — its stderr IS the diagnosis, not the JSON
            err = (scan.stderr or scan.stdout).strip().splitlines()
            detail = err[0][:200] if err else f"exit {scan.returncode} with no output"
            return fail(f"the scan output was not the findings JSON contract ({detail})")
        kinds = {a.kind for a in actions if a.severity == "fail"}
        missing = [k for k in EXPECTED_FAIL_KINDS if k not in kinds]
        if scan.returncode == 0 or missing:
            return fail(f"the scan missed expected fail findings: {missing or 'none found'}")
        return 0

    def fix_step(self) -> int:
        scan = run(self.cmd + ["--repo", str(self.project), "--json"], cwd=self.project)
        actions = parse_actions(scan.stdout)
        if actions is None:
            return fail("the scan output was not the findings JSON contract")
        for a in actions:
            m = re.search(r"fix: (lucidlint fix --kind \S+ --file \S+ --line \d+)", a.message)
            if not m:
                continue
            cmd = self.cmd + m.group(1).split()[1:]
            if a.kind == "magic-number":
                cmd += ["--name", CONST_NAME]
            fixed = run(cmd, cwd=self.project)
            if fixed.returncode != 0:
                return fail(f"fix {a.kind} failed: {(fixed.stderr or fixed.stdout).strip()}")
        return 0

    def verify_step(self) -> int:
        rescan = run(self.cmd + ["--repo", str(self.project), "--json"], cwd=self.project)
        actions = parse_actions(rescan.stdout)
        if actions is None:
            return fail("the re-scan output was not the findings JSON contract")
        remaining = [a.kind for a in actions if a.severity == "fail"]
        if rescan.returncode != 0 or remaining:
            return fail(f"findings remained after fixes: {remaining or 'unknown'}")
        print("deploy-check: scan found the expected findings, every fix applied, re-scan clean")
        return 0


def main() -> int:
    ap = argparse.ArgumentParser(prog="deploy-check")
    ap.add_argument(
        "--lucidlint",
        required=True,
        help="shlex-split command that runs lucidlint (a binary path, or `python3 <bundle>/lucidlint.py`)",
    )
    ap.add_argument("--project", required=True, help="where to generate the mini project")
    args = ap.parse_args()

    cmd = shlex.split(args.lucidlint)
    if not cmd:
        raise SystemExit("deploy-check: --lucidlint is empty")
    project = Path(args.project)
    project.mkdir(parents=True, exist_ok=True)
    for rel, content in PROJECT_FILES.items():
        (project / rel).write_text(content)

    check = _DeployCheck(cmd, project)
    status = check.find_step()
    if status:
        return status
    status = check.fix_step()
    if status:
        return status
    return check.verify_step()


if __name__ == "__main__":
    sys.exit(main())
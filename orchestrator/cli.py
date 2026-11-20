"""The CLI: the one entry the shell, the wheel and the release bundle call.

`main` is the process's whole behaviour — parse the arguments, resolve the
repo, and hand the run to the fix command or the gate. The entry module keeps
the `lucidlint:main` hook and the deps/ bootstrap; the parsing and the
dispatch live here.
"""

from __future__ import annotations

import argparse
from pathlib import Path

from orchestrator.fix.fix_command import FixCommand
from orchestrator.gate_runner import GateRunner
from orchestrator.host import Host
from orchestrator.services import Services
from orchestrator.version import VERSION


class Cli:
    """The command line: one parsed request, one dispatch. The arguments are
    its state, so the run reads them instead of re-parsing."""

    def __init__(self, args: argparse.Namespace, host: Host | None = None) -> None:
        self.args = args
        self.repo = args.repo.resolve()
        self.host = host or Host.default()
        self.services = Services(self.host)

    @staticmethod
    def parse_args() -> argparse.Namespace:
        p = argparse.ArgumentParser(
            prog="lucidlint",
            description=(
                "deterministic code health for Python and Rust: a compiled scan core "
                "under a thin orchestrator. Run the gate with no subcommand; apply "
                "deterministic fixes with the `fix` subcommand (finding messages "
                "carry the exact command)."
            ),
        )
        p.add_argument(
            "--version",
            action="version",
            version=f"lucidlint {VERSION}",
            help="print the version and exit",
        )
        p.add_argument("--repo", type=Path, default=Path.cwd(), help="repository root (default: cwd)")
        p.add_argument(
            "--file",
            type=str,
            default=None,
            help="scan ONE repo-relative file (the LSP mode): per-file findings only, "
            "no git history / graph / coverage / repo-wide scans",
        )
        p.add_argument("--include-tests", action="store_true", help="also analyze test files/nodes")
        p.add_argument(
            "--baseline",
            type=Path,
            default=None,
            help="baseline JSON of acknowledged actions; listed actions are reported but do not fail the gate",
        )
        p.add_argument(
            "--update-baseline",
            action="store_true",
            help="write all current action keys to --baseline and exit 0 (lock the list, like pyrefly baselines)",
        )
        p.add_argument("--json", action="store_true", help="emit actions as JSON object (meta + actions) on stdout")
        p.add_argument(
            "--refresh-coverage",
            action="store_true",
            help="run the repo's coverage suite (make coverage) before scanning so coverage verdicts are fresh (slow)",
        )
        p.add_argument("--warn", action="store_true", help="exit 0 even when actions exist (informational run)")

        sub = p.add_subparsers(dest="command", metavar="COMMAND")
        fix = sub.add_parser(
            "fix",
            help="apply a deterministic fix for one finding (the finding's message carries the exact command)",
        )
        Cli._text_option(
            fix,
            "kind",
            "the fix family (from the finding message: magic-number, extract-method, stale-suppression, ...)",
        )
        Cli._text_option(fix, "file", "repo-relative file for the finding")
        fix.add_argument("--line", type=int, default=0, help="line of the finding (omitted when the file has one)")
        Cli._text_option(
            fix,
            "name",
            "the semantic name the tool cannot invent: a constant for magic-number, "
            "the extracted method/class name",
        )
        Cli._text_option(
            fix,
            "callee",
            "the callee whose --params these are, when the finding's message names it "
            "(binds the fix on lines with nested calls)",
        )
        Cli._text_option(
            fix,
            "params",
            "comma-separated callee parameter names for positional-literals (external callees)",
        )
        Cli._switch(
            fix,
            "confirm",
            "explicitly apply a previewed structural fix (the name IS the commitment for extract-method)",
        )
        return p.parse_args()

    @staticmethod
    def _text_option(parser: argparse.ArgumentParser, name: str, help_text: str) -> None:
        """A `--<name> <text>` option with no default."""
        parser.add_argument(f"--{name}", type=str, default=None, help=help_text)

    @staticmethod
    def _switch(parser: argparse.ArgumentParser, name: str, help_text: str) -> None:
        """A `--<name>` boolean switch."""
        parser.add_argument(f"--{name}", action="store_true", help=help_text)

    def run(self) -> int:
        """Dispatch the one command the arguments name."""
        if self.args.command == "fix":
            return self._fix()
        return self._gate()

    def _fix(self) -> int:
        return FixCommand(self.args, self.repo, self.host, self.services).run()

    def _gate(self) -> int:
        return GateRunner(self.repo, self.args, self.host, self.services).run()

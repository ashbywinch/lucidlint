"""The CLI's two output doors: the stderr warning line, and the trimmed diff a
fix prints for review.

One owner, so the report's out-of-band output has one format. Everything else
the gate prints is part of a report (`render`) — these two are the lines that
sit outside it. The diff's head length is the owner's state.
"""

from __future__ import annotations

import difflib
import sys


class Console:
    """The out-of-band lines: warnings on stderr, and the diff after a write.
    The diff is trimmed to its head, and that bound is the console's."""

    def __init__(self, diff_head: int) -> None:
        self.diff_head = diff_head

    def warn(self, msg: str) -> None:
        print(msg, file=sys.stderr)

    def print_diff(self, before: str, after: str, rel: str) -> None:
        """The applied change AS A DIFF — a fix the agent cannot review is a fix
        the agent cannot trust (the houses sweep's 'always git diff after fix'
        advice existed because the apply confirmation was a bare line)."""
        diff = list(
            difflib.unified_diff(
                before.splitlines(),
                after.splitlines(),
                fromfile=f"{rel} (before)",
                tofile=f"{rel} (after)",
                lineterm="",
            )
        )
        if not diff:
            return
        if len(diff) > self.diff_head:
            diff = diff[: self.diff_head] + [f"... ({len(diff) - self.diff_head} more diff lines omitted)"]
        print("\n".join(diff))


CONSOLE = Console(diff_head=40)

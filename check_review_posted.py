# code-health: ignore-file class-module the class is a small HTTP-redirect helper
# (urllib's auto-follow would leak the Authorization header to the signed blob
# host); the script is one unit with one reason to change, not a model module.
"""Called by .github/workflows/pr-agent.yml — fail the PR if the review bot
did not post a "PR Reviewer Guide" comment covering the head commit.
The review may have failed silently; this check prevents merging unreviewed.

A comment covers the head commit when its body references the commit's SHA
(the incremental-review form, "Starting from commit .../<SHA>") or it was
posted after the head commit landed (the first review on a PR is posted
without the SHA marker — observed: pr-agent v0.41.1 regular reviews never
contain the head SHA). The range-start SHA in the incremental body is the
first NEW commit, not the head — coverage still falls to the posted-after
rule; the explicit form is belt-and-braces for reviews that embed it.

Why the bot's own step cannot fail (2026-08-11, read from the v0.41.1
source): ``PRAgent.handle_request`` catches EVERY exception with a bare
``except`` — it logs "Failed to process the command." plus the traceback
and returns False — and ``github_action_runner.py`` discards that return
value, so the action exits 0 whether or not the review happened. A green
bot step is therefore meaningless, and the only truthful signal is this
check. To make the failure SELF-EXPLAINING rather than a guess, the check
fetches the run's own log (the checks API) and extracts the bot's error
lines: the "Failed to process the command." traceback, model/API errors,
and the diff-token-cap marker — the check's failure message names the
reason instead of "may have failed silently".
"""

import json
import os
import re
import sys
import urllib.request
from urllib.error import HTTPError

BOT_STEP_MARK = "Run the-pr-agent/pr-agent"

# The markers the bot leaves when a review dies inside the (swallowed)
# exception seam, plus the size signal that precedes a died model call.
_ERROR_MARKERS = (
    "Failed to process the command.",
    "Traceback (most recent call last)",
)
_SIZE_RE = re.compile(r"Tokens:\s*(\d+),\s*total tokens over limit:\s*(\d+)")
_TRACEBACK_TAIL = 12  # lines after a traceback/marker worth reporting


def _get(url: str, token: str, accept: str = "application/vnd.github+json") -> bytes:
    req = urllib.request.Request(
        url,
        headers={"Authorization": f"Bearer {token}", "Accept": accept},
    )
    return urllib.request.urlopen(req).read()


def _get_json(url: str, token: str):
    return json.loads(_get(url, token))


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """The log endpoint 302s to a signed blob URL; urllib's auto-follow
    forwards the Authorization header onto the blob host, which rejects it
    (401 InvalidAuthenticationInfo, 2026-08-11). Stop at the 302 and fetch
    the signed Location bare."""

    # urllib's HTTPRedirectHandler.redirect_request override requires this
    # exact signature — the framework defines the contract
    # code-health: ignore long-param-list framework-mandated override signature
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


_OPENER = urllib.request.build_opener(_NoRedirect())


def _job_log(repo: str, token: str) -> str | None:
    """The running job's own accumulated log (the bot step's output), or
    None when the checks API won't serve it mid-run — the caller falls back
    to the generic message. Requires the workflow's ``actions: read``."""
    run_id = os.environ.get("GITHUB_RUN_ID")
    if not run_id:
        return None
    try:
        jobs = _get_json(f"https://api.github.com/repos/{repo}/actions/runs/{run_id}/jobs", token)
        if not jobs.get("jobs"):
            return None
        req = urllib.request.Request(
            f"https://api.github.com/repos/{repo}/actions/jobs/{jobs['jobs'][0]['id']}/logs",
            headers={"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json"},
        )
        try:
            return _OPENER.open(req).read().decode("utf-8", errors="replace")
        except HTTPError as e:
            if e.code != 302:
                raise
            return urllib.request.urlopen(e.headers["Location"]).read().decode("utf-8", errors="replace")
    except (HTTPError, KeyError, ValueError):
        return None


def _bot_failure_reason(repo: str, token: str) -> str:
    """The bot's own words about its failure, from the run log — the reason
    the review died, instead of a guess. Falls back to the last thing the
    bot said."""
    log = _job_log(repo, token)
    if log is None:
        return "the bot's log could not be read from the checks API"
    # The raw job blob is TIMESTAMPZ + message lines with no step prefixes
    # (the runner's processed view adds them); the bot's own records are the
    # structured JSON lines with a "text" field — those carry the markers.
    bot_lines = [ln for ln in log.splitlines() if '"text"' in ln]
    if not bot_lines:
        return "the bot's step produced no log output"

    return _failure_reason(bot_lines)


def _failure_reason(bot_lines: list[str]) -> str:
    """Why the bot died, from its own log lines: the diff-token cap (the
    size signal that precedes a died model call), the swallowed traceback
    seam, or the last thing it said."""
    for ln in bot_lines:
        m = _SIZE_RE.search(ln)
        if m:
            return (
                f"the PR's cumulative diff is {m.group(1)} tokens — over the bot's "
                f"{m.group(2)}-token review cap; the review died after pruning the diff"
            )
    for i, ln in enumerate(bot_lines):
        if any(mark in ln for mark in _ERROR_MARKERS):
            tail = [line for line in bot_lines[i : i + _TRACEBACK_TAIL] if line.strip()]
            snippet = " | ".join(line.split("\t")[-1][:200] for line in tail[:4])
            return f"the bot logged an error before dying: {snippet}"
    last = next((line for line in reversed(bot_lines) if line.strip()), "")
    return f"the bot's last output before going silent: {last.split(chr(9))[-1][:200]}"


def main() -> int:
    sha = os.environ["SHA"]
    repo = os.environ["GITHUB_REPOSITORY"]
    pr_number = os.environ["PR_NUMBER"]
    token = os.environ["GITHUB_TOKEN"]

    try:
        commit = _get_json(f"https://api.github.com/repos/{repo}/commits/{sha}", token)
    except HTTPError as e:
        # surfaces via the ::error line + the non-zero exit — not a swallow
        print(f"::error::commit {sha} is not fetchable ({e.code}) — the review cannot cover it.")
        return 1
    head_committed_at = commit["commit"]["committer"]["date"]

    comments = _get_json(f"https://api.github.com/repos/{repo}/issues/{pr_number}/comments", token)
    # the review posts with the regular header ("## PR Reviewer Guide") or
    # the incremental form ("## Incremental PR Reviewer Guide" — the -i
    # path, 2026-08-11: the first incremental run posted exactly that and
    # the check missed it, failing a review that had succeeded)
    covered = any(
        c.get("body", "").startswith(("## PR Reviewer Guide", "## Incremental PR Reviewer Guide"))
        and (
            sha in c.get("body", "")
            or f"commit/{sha}" in c.get("body", "")  # the incremental "Starting from commit .../<SHA>" form
            or c.get("created_at", "") >= head_committed_at
        )
        for c in comments
    )
    if not covered:
        reason = _bot_failure_reason(repo, token)
        print(f"::error::AI review did not post for commit {sha} — {reason}.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

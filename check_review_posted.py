"""Called by .github/workflows/pr-agent.yml — fail the PR if the review bot
did not post a "PR Reviewer Guide" comment covering the head commit.
The review may have failed silently; this check prevents merging unreviewed.

A comment covers the head commit when its body references the commit's SHA
(the incremental-review form, "Starting from commit .../<SHA>") or it was
posted after the head commit landed (the first review on a PR is posted
without the SHA marker — observed: pr-agent v0.41.1 regular reviews never
contain the head SHA).
"""

import json
import os
import sys
import urllib.request


def _get_json(url: str, token: str):
    req = urllib.request.Request(
        url,
        headers={"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json"},
    )
    return json.load(urllib.request.urlopen(req))


def main() -> int:
    sha = os.environ["SHA"]
    repo = os.environ["GITHUB_REPOSITORY"]
    pr_number = os.environ["PR_NUMBER"]
    token = os.environ["GITHUB_TOKEN"]

    commit = _get_json(f"https://api.github.com/repos/{repo}/commits/{sha}", token)
    head_committed_at = commit["commit"]["committer"]["date"]

    comments = _get_json(f"https://api.github.com/repos/{repo}/issues/{pr_number}/comments", token)
    covered = any(
        c.get("body", "").startswith("## PR Reviewer Guide")
        and (sha in c.get("body", "") or c.get("created_at", "") >= head_committed_at)
        for c in comments
    )
    if not covered:
        print(f"::error::AI review did not post for commit {sha} — the review may have failed silently.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

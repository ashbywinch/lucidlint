# build-tools

Shared CI utilities used by the house repos' workflows. Each tool is
self-contained (stdlib only), reads its inputs from environment variables,
and is fetched by the consuming workflow at run time — e.g. the pr-agent
"AI Code Review" workflow fetches `check_review_posted.py` from the pinned
tag and runs it as its review-attribution gate.

## Tools

- `check_review_posted.py` — fail the PR when the AI review bot did not post
  a "PR Reviewer Guide" comment covering the head commit. Env: `SHA`,
  `GITHUB_REPOSITORY`, `PR_NUMBER`, `GITHUB_TOKEN`. Attribution: the comment
  body references the head SHA (incremental reviews), or the comment was
  created after the head commit landed (first-review case — regular pr-agent
  reviews never contain the SHA).

## Use in a workflow

```yaml
- name: Fail loud if no review covers the head commit
  env:
    SHA: ${{ github.event.pull_request.head.sha }}
    PR_NUMBER: ${{ github.event.pull_request.number }}
    GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
  run: |
    curl -fsSL -o check_review_posted.py \
      https://raw.githubusercontent.com/ashbywinch/build-tools/v1/check_review_posted.py
    python3 check_review_posted.py
```

Pin the URL to a tag, never `main`, so a later change cannot silently alter
what CI runs.

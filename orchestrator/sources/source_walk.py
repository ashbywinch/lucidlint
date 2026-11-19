"""The repo's own source list and its documentation set.

Every file the scan reads is git's answer, not an invented walk: tracked
files plus untracked-not-ignored, which is exactly what the repo's .gitignore
defines as its code. pygit2 is the fast path; git itself is the fallback; a
git-less tree falls back to the rglob walk. The docs harvest asks git the
same way, and treats ignored markdown as intentionally absent.

The walk is one repo's walk, so the repo is its state and the callers no
longer restate it at every call site.
"""

from __future__ import annotations

import os
import re
from pathlib import Path

from orchestrator.console import CONSOLE
from orchestrator.host import HOST
from orchestrator.sources.source_file import SourceFile

# exact env/tool directories that hide vendored code; the .venv* / venv* /
# .conda* families are matched by prefix in `excluded_part`
EXCLUDED_DIRS = frozenset(
    {
        ".git",
        ".venv",
        "node_modules",
        "__pycache__",
        "dist",
        "build",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
    }
)


class SourceWalk:
    """The source files and documentation set of one repo."""

    def __init__(self, repo: Path) -> None:
        self.repo = repo

    @staticmethod
    def excluded_part(part: str) -> bool:
        """An env/tool directory hides vendored code — .venv-htr, .conda-tools,
        node_modules, .mypy_cache, etc. Exact names in EXCLUDED_DIRS plus the
        .venv* / venv* env-dir families (a repo's real modules never live there)."""
        env_prefixes = (".venv", "venv", ".conda")
        return part in EXCLUDED_DIRS or part.startswith(env_prefixes)

    @staticmethod
    def is_test_path(rel: str) -> bool:
        parts = rel.split("/")
        return (
            "tests" in parts
            or "__tests__" in parts
            or any(part.startswith("test_") or part.endswith("_test.py") for part in parts[-1:])
        )

    @staticmethod
    def md_rels(stdout: str) -> tuple[str, ...]:
        """The markdown paths in a NUL-separated git path list."""
        return tuple(sorted(p for p in stdout.split("\0") if p.endswith(".md")))

    def git_ignored_md(self) -> tuple[str, ...]:
        """The .md paths git itself reports as ignored — one `git ls-files` call,
        no filesystem walk. `--exclude-standard` honours .gitignore,
        .git/info/exclude and the global excludes file, and git does not descend a
        nested checkout (another worktree of this repo is a single opaque entry),
        so a sibling checkout's markdown is never enumerated. Empty when git or
        the repository is missing — nothing is ignored then."""
        try:
            proc = HOST.subprocess.run(
                [
                    "git",
                    "-C",
                    str(self.repo),
                    "ls-files",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "-z",
                    "--",
                    "*.md",
                ],
                capture_output=True,
                text=True,
                timeout=30,
            )
        # TimeoutExpired is NOT an OSError: the timeout below is the reason a hung
        # git is caught here at all
        except (
            OSError,
            HOST.subprocess.TimeoutExpired,
        ) as e:  # lucidlint: ignore swallow git missing — nothing is ignored
            CONSOLE.warn(f"git ls-files --ignored: {e}")
            return ()
        if proc.returncode != 0:
            return ()
        return self.md_rels(proc.stdout)

    def repo_md_paths(self) -> tuple[str, ...]:
        """The repo's own .md files — git's answer (tracked plus
        untracked-not-ignored), the enumeration `py_files` uses. EVERY doc the
        repo owns is enumerated, wherever it lives: the harvest must not decide
        which markdown counts by re-deriving the scan's roots. Ignored docs are
        not the shipped doc set and a nested checkout is one opaque entry to git,
        so neither is enumerated — and a vendored tree is ignored, which is what
        `--exclude-standard` answers."""
        try:
            proc = HOST.subprocess.run(
                [
                    "git",
                    "-C",
                    str(self.repo),
                    "ls-files",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                    "-z",
                    "--",
                    "*.md",
                ],
                capture_output=True,
                text=True,
                timeout=30,
            )
        except (
            OSError,
            HOST.subprocess.TimeoutExpired,
        ) as e:  # lucidlint: ignore swallow git missing — nothing harvested
            CONSOLE.warn(f"git ls-files: {e}")
            return ()
        if proc.returncode != 0:
            return ()
        return self.md_rels(proc.stdout)

    def gitignored_docs(self) -> tuple[str, ...]:
        """Repo-relative .md paths git ignores — private docs (review-log R3). The
        docs scan must treat them as intentionally absent: they are not shipped,
        so a link to one is not a broken link and an orphan one is not an
        undiscoverable doc. Both halves are git's answer: the ignored files from
        `ls-files`, the referenced-but-absent ones from `check-ignore` on the
        references the repo's own docs make (a private doc may never have been
        committed, so no enumeration can see it — only the reference does)."""
        ignored = set(self.git_ignored_md())
        refs: set[str] = set()
        for rel in self.repo_md_paths():
            p = self.repo / rel
            try:
                text = p.read_text(encoding="utf-8", errors="replace")
            # a dangling symlink or an unreadable doc is not a reason to fail the
            # scan — one stale link used to kill the whole run
            except OSError:  # lucidlint: ignore swallow unreadable doc — it can carry no reference
                continue
            for hit in re.findall(r"`([^`]+\.md)`|\[[^\]]*\]\(([^)]+\.md)\)", text):
                ref = hit[0] or hit[1]
                # a parent-relative or absolute reference lies OUTSIDE the
                # repo — `git check-ignore` aborts the whole batch on one
                # (rc 128, no output); those are never repo-ignored anyway
                if ref.startswith(("../", "/")):
                    continue
                # references resolve like the Rust docs scan: `docs/`- and
                # `standards/`-prefixed names against the repo ROOT, anything
                # else (a sibling bare name) against the doc's own directory —
                # `other.md` from docs/sub/guide.md is docs/sub/other.md, not
                # repo-root other.md (review bot)
                cand = self.repo / ref if ref.startswith(("docs/", "standards/")) else p.parent / ref
                try:
                    refs.add(cand.resolve().relative_to(self.repo).as_posix())
                except ValueError:
                    continue  # resolved outside the repo
        if refs:
            try:
                proc = HOST.subprocess.run(
                    ["git", "-C", str(self.repo), "check-ignore", "--stdin"],
                    input="\n".join(sorted(refs)),
                    capture_output=True,
                    text=True,
                )
            # no git binary — the referenced-absent-doc query degrades; the
            # ls-files answer still stands (gitless mode, review bot)
            except OSError as e:  # lucidlint: ignore swallow terminal boundary — gitless env, no caller to propagate to
                CONSOLE.warn(f"git check-ignore unavailable ({e}) — referenced-absent docs stay visible")
            else:
                for line in proc.stdout.splitlines():
                    line = line.strip()
                    if line.endswith(".md"):
                        ignored.add(line.replace(os.sep, "/"))
        return tuple(sorted(ignored))

    def rglob_sources(self) -> list[SourceFile]:
        """The fallback source list when git is unavailable."""
        return [
            SourceFile(py, py.relative_to(self.repo).as_posix())
            for py in sorted(self.repo.rglob("*.py"))
            + sorted(self.repo.rglob("*.rs"))
            + sorted(self.repo.rglob("*.md"))
            if not any(self.excluded_part(part) for part in py.parts)
        ]

    def py_files(self, only_rel: str | None = None) -> list[SourceFile]:
        """The repo's own .py and .rs files — git's answer, not an invented list.

        `git ls-files --cached --others --exclude-standard` = tracked files plus
        untracked-not-ignored: exactly what the repo's .gitignore defines as its
        code (venvs, caches, generated output never qualify, whatever they are
        named). One call per run, NUL-split, no quoting issues. With only_rel
        set, returns just that file (the --file / LSP mode).
        """
        repo = self.repo
        if only_rel is not None:
            py = repo / only_rel
            return [SourceFile(py, only_rel)] if py.is_file() and py.suffix in (".py", ".rs", ".md") else []
        if HOST.pygit2 is None or not (repo / ".git").exists():
            # no pygit2 (CI runners, bare pythons): ask git itself — the answer
            # honors .gitignore, so an ignored dir (a repo's own .tools/lucidlint
            # bundle, a venv) is never scanned. rglob only when git is also
            # unavailable — a git-less repo has nothing ignored to respect.
            if (repo / ".git").exists():
                try:
                    proc = HOST.subprocess.run(
                        ["git", "-C", str(repo), "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
                        capture_output=True,
                        text=True,
                        timeout=30,
                    )
                    if proc.returncode == 0:
                        rels = [
                            p
                            for p in proc.stdout.split("\0")
                            if p.endswith((".py", ".rs", ".md")) and not p.startswith("tests/fixtures/")
                        ]
                        return [SourceFile(repo / rel, rel) for rel in sorted(rels)]
                # lucidlint: ignore swallow git missing or unrunnable — degrade to the rglob walk rather than crash
                except Exception as e:
                    CONSOLE.warn(f"git ls-files: {e}")
            return self.rglob_sources()  # no git — certain: silent fallback
        try:
            r = HOST.pygit2.Repository(str(repo))
            tracked = {e.path for e in r.index if e.path.endswith((".py", ".rs", ".md"))}
            # Untracked non-ignored files: walk working tree
            untracked = set()
            for root, dirs, files in os.walk(repo):
                if ".git" in dirs:
                    dirs.remove(".git")
                for f in files:
                    if f.endswith((".py", ".rs", ".md")):
                        full = Path(root) / f
                        rel = str(full.relative_to(repo))
                        if rel in tracked:
                            continue
                        try:
                            if not r.path_is_ignored(rel):
                                untracked.add(rel)
                        except ValueError:  # lucidlint: ignore swallow ambiguous path — treat as untracked
                            untracked.add(rel)
            rels = sorted(r for r in (tracked | untracked) if not r.startswith("tests/fixtures/"))
            return [SourceFile(repo / rel, rel) for rel in rels]
        except KeyError:
            return self.rglob_sources()  # not a git repo — certain: silent fallback
        except Exception as e:
            CONSOLE.warn(f"file list: {e}")  # unexpected — show the actual error
            return self.rglob_sources()


def gitignored_docs(repo: Path) -> tuple[str, ...]:
    """The repo's ignored docs — the module's one-shot entry for a caller that
    has the repo and nothing else."""
    return SourceWalk(repo).gitignored_docs()

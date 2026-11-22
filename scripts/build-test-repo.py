#!/usr/bin/env python3
"""Build the canonical lucidlint test repo and store it as a fixture.

The repo history is DETERMINISTIC: fixed commit timestamps and a fixed
signature, so churn counts and last-modified dates asserted by the tests
never drift. The output is `tests/fixtures/test-repo.tar.gz` — a real
pygit2 repository (working tree + .git) that tests extract and open.

History (all timestamps fixed at epoch offsets):
  c1  houses/app.py          (base)
  c2  houses/app.py  modify  (churn 2 for app.py)
  c3  scripts/oneoff.py      (churn 1)
  c4  tests/unit/test_app.py (churn 1)
The branch `other` pins c1 — tests that need a branch diff create their
own commits on main after branching.

Run via: `make test-fixture` (regenerates the tarball).
"""
# lucidlint: ignore-file global-state test-repo builder constants are one-off configuration

from __future__ import annotations

import io
import shutil
import tarfile
from pathlib import Path
from pathlib import Path as _CodePath

import pygit2

_CODE_FIXTURES = _CodePath(__file__).resolve().parent.parent / "tests" / "fixtures" / "code"


def code_fixture(name: str) -> str:
    """Read a code sample kept in a real file under tests/fixtures/code/."""
    return (_CODE_FIXTURES / name).read_text()


ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "tests" / "fixtures" / "test-repo.tar.gz"

# 2026-08-01 — inside the churn window (730d), fixed for determinism
SIG = pygit2.Signature("CodeHealth Test", "lucidlint@example.com", time=1785542400, offset=0)


def commit_all(repo: pygit2.Repository, files: dict[str, str], message: str, parents) -> pygit2.Oid:
    for rel, content in files.items():
        p = ROOT / ".test-repo" / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content)
    repo.index.add_all()
    tree = repo.index.write_tree()
    return repo.create_commit("HEAD", SIG, SIG, message, tree, parents)


def build() -> None:
    work = ROOT / ".test-repo"
    shutil.rmtree(work, ignore_errors=True)
    (work / "houses").mkdir(parents=True)
    (work / "scripts").mkdir(parents=True)
    (work / "tests" / "unit").mkdir(parents=True)

    repo = pygit2.init_repository(str(work))
    c1 = commit_all(repo, {"houses/app.py": code_fixture("build__f9694aad.py")}, "base: houses", [])
    c2 = commit_all(
        repo,
        {"houses/app.py": code_fixture("build__269ea9ca.py")},
        "modify: houses/app.py",
        [repo.head.target],
    )
    c3 = commit_all(repo, {"scripts/oneoff.py": code_fixture("build__37868550.py")}, "add: scripts", [repo.head.target])
    c4 = commit_all(
        repo, {"tests/unit/test_app.py": code_fixture("build__31d3b45e.py")}, "add: tests", [repo.head.target]
    )
    assert all((c1, c2, c3, c4))

    # tar the working tree + .git (not the parent dir)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tf:
        for p in sorted(work.rglob("*")):
            if p.is_file():
                tf.add(p, arcname=p.relative_to(work))
    OUT.write_bytes(buf.getvalue())
    shutil.rmtree(work, ignore_errors=True)
    print(f"wrote {OUT} ({OUT.stat().st_size} bytes)")


if __name__ == "__main__":
    build()

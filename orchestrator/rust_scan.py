"""The Rust scan core — the required finding engine.

One binary invocation per repo per run (or per file in --file / LSP mode); the
versioned JSON findings ARE the action model (kind/severity owned by the
core); this driver converts them to Actions verbatim and fills the
orchestrator's fields (churn, priority, provenance). Everything the driver
needs to know about one invocation — the binary it found, the flags, the
finding-to-path mapping — is its own.
"""

from __future__ import annotations

import json
import os
from collections import Counter
from pathlib import Path
from subprocess import SubprocessError
from typing import Any

from orchestrator.artifact import artifact_root
from orchestrator.action import Action
from orchestrator.console import CONSOLE
from orchestrator.host import Host
from orchestrator.rust_finding import RustFinding
from orchestrator.rust_findings import RustFindings
from orchestrator.scan_flags import ScanFlags
from orchestrator.scan_inputs import INPUTS
from orchestrator.sources.coverage_input import CoverageInput
from orchestrator.sources.coverage_request import CoverageRequest
from orchestrator.sources.file_history import FileHistory
from orchestrator.sources.source_file import SourceFile
from orchestrator.sources.source_walk import SourceWalk, gitignored_docs


class RustScan:
    """The Rust scan core — the required finding engine.

    One binary invocation per repo per run (or per file in --file / LSP mode);
    the versioned JSON findings ARE the action model (kind/severity owned by
    the core); this driver converts them to Actions verbatim and fills the
    orchestrator's fields (churn, priority, provenance)."""

    # the test-only families: a test file is excluded from the health scan,
    # but the rules that live in tests are scanned for alone
    TEST_ONLY_SIGNALS = frozenset({"suppression", "type-ignore", "monkeypatch", "skipif", "fakefs", "allow-reason"})

    # a scanner run that fails to start, hangs, or answers with an unreadable
    # contract is "Rust unavailable" — the caller fail-fasts, never a silent pass
    SCANNER_FAILURES = (OSError, SubprocessError, json.JSONDecodeError, ValueError)

    @staticmethod
    def warn(msg: str) -> None:
        """The driver's warning line — one door, so the driver's collaboration
        with the console is one edge and its functions speak to their own
        module."""
        CONSOLE.warn(msg)

    def __init__(self, host: Host, graph: Any) -> None:
        # the graph contract is another service's work; the scan asks for the
        # export path rather than reaching for a process-wide registry. A scan
        # with no collaborator runs without the graph families.
        self._host = host
        self.graph = graph
        self._cache: dict[tuple[Path, tuple[str, ...]], RustFindings | None] = {}
        self._binary_cache: dict[Path, Path | None] = {}
        self._pending_gitignored: tuple[str, ...] = ()
        self._pending_graph: Path | None = None
        self._pending_churn: Path | None = None
        self._pending_coverage: Path | None = None
        self._pending_tests: bool = False
        self._pending_docs: str | None = None

    def binary(self, repo: Path) -> Path | None:
        """The scan binary: env override, then the repo's own build, then the
        tool checkout's build, then the distribution bundle's sibling binary
        (a `lucidlint` release installs as <prefix>/bin/lucidlint next to
        lucidlint.py — the bundle is self-contained, no env needed).
        None when not built — the Python path takes over. A LOCAL build
        (scanner/target/release) whose sources are newer is treated as
        missing: a stale core would silently test old rules — make the
        forced rebuild (`make test`, `make self-check`) or refuse loudly
        instead. env-overridden and bundled binaries are never refused —
        they are deliberate pins."""
        if repo in self._binary_cache:
            return self._binary_cache[repo]
        exe = ".exe" if os.name == "nt" else ""
        found = next((p for p in self._scanner_candidates(repo, exe) if p.is_file()), None)
        if found is not None and self._scanner_build_is_stale(found):
            # a local build whose sources are newer — refusing beats testing
            # old rules; env-pinned and bundled binaries are never refused
            RustScan.warn(
                f"the scanner binary at {found} is stale — scanner sources are newer; "
                "rebuild with `make scanner-check` before running tests or the gate"
            )
            found = None
        self._binary_cache[repo] = found
        return found

    def load(
        self,
        repo: Path,
        files: list[SourceFile],
        flags: ScanFlags | None = None,
    ) -> RustFindings | None:
        """Findings per rel for one file set; None = Rust unavailable (Python path)."""
        if flags is None:
            flags = self._flags()
        rels = tuple(sf.rel for sf in files)
        key = (repo, rels)
        if key in self._cache:
            return self._cache[key]
        binary = self.binary(repo)
        if binary is None:
            raise RuntimeError("the Rust scan core is required — build it with `make scanner-check`")
        result: dict[str, list[RustFinding]] | None = None
        cc_result: dict[str, list[tuple[str, int, int]]] = {}
        header = ""
        suppression_census: dict[str, int] | None = None
        if not files:
            result = {}  # a repo with no .py/.rs files scans clean — GATE: PASS, not an error
        else:
            result = {}
            try:
                cmd = [str(binary), "--root", str(repo.resolve())]
                if flags.graph is not None:
                    cmd += ["--graph", str(flags.graph)]
                if flags.churn_json is not None:
                    cmd += ["--churn", str(flags.churn_json)]
                if flags.coverage is not None:
                    cmd += ["--coverage", str(flags.coverage)]
                if flags.include_tests:
                    cmd.append("--include-tests")
                if flags.docs_root is not None:
                    cmd += ["--docs", flags.docs_root]
                if flags.gitignored:
                    cmd += ["--gitignored", json.dumps(list(flags.gitignored))]
                # pass REPO-RELATIVE paths (the binary runs with the repo as
                # its cwd): findings — and the fix: directives they carry —
                # stay repo-relative, matching the Action model
                cmd += [str(sf.rel) for sf in files]
                proc = self._host.process.run(cmd, capture_output=True, text=True, timeout=300, cwd=str(repo))
                if proc.returncode == 0:
                    data = json.loads(proc.stdout)
                    if data.get("schema_version") != 4:
                        raise RuntimeError(
                            f"scanner contract schema {data.get('schema_version')} — expected 4; "
                            "rebuild the binary (make scanner-check)"
                        )
                    header = str(data.get("header", ""))
                    suppression_census = {str(k): int(v) for k, v in (data.get("suppressions") or {}).items()}
                    rels_set = set(rels)
                    unmapped: set[str] = set()
                    for f in data.get("findings", []) + data.get("complexity", []):
                        rel = self._rust_finding_rel(f.get("file", ""), repo, rels_set)
                        if rel is None:
                            unmapped.add(str(f.get("file", "")))
                            continue
                        result.setdefault(rel, []).append(
                            RustFinding(
                                kind=f.get("kind", "standard"),
                                signal=f.get("signal", f.get("kind", "")),
                                severity=f.get("severity", "fail"),
                                file=rel,
                                line=int(f.get("line", 0)),
                                col=int(f.get("col", 0)),
                                function=f.get("function", ""),
                                message=f.get("message", ""),
                                metric=float(f.get("metric", 1.0)),
                                seam_members=tuple(str(m) for m in f.get("seam_members", [])),
                                fix_kind=str(f.get("fix_kind", "") or ""),
                            )
                        )
                    for e in data.get("cc", []):
                        rel = self._rust_finding_rel(e.get("file", ""), repo, rels_set)
                        if rel is None:
                            unmapped.add(str(e.get("file", "")))
                            continue
                        cc_result.setdefault(rel, []).append(
                            (e.get("function", ""), int(e.get("line", 0)), int(e.get("cc", 0)))
                        )
                    if unmapped:
                        RustScan.warn(self._unmapped_note(unmapped))
                else:
                    result = None
            except RustScan.SCANNER_FAILURES:  # lucidlint: ignore swallow degraded runs report nothing — visible
                result = None
        # the binary has read them; they are not needed again this run
        INPUTS.release(self._pending_churn)
        INPUTS.release(self._pending_coverage)
        self._pending_churn = None
        self._pending_coverage = None
        wrapped = RustFindings(result, cc_result, header, suppression_census) if result is not None else None
        self._cache[key] = wrapped
        return wrapped

    def prepare(
        self,
        repo: Path,
        only_rel: str | None,
        include_tests: bool,
        file_churn: Counter[str],
        coverage: CoverageInput | None = None,
    ) -> None:
        """Graph contract + churn JSON + coverage snapshot + docs root for the
        next scan; repo-wide only. Each run scans fresh — the per-run cache
        must not leak across main() calls (the tests drive several runs in one
        process)."""
        self._cache.clear()
        self._pending_graph = None
        self._pending_churn = None
        self._pending_coverage = None
        self._pending_tests = include_tests
        self._pending_docs = None
        self._pending_gitignored = ()
        if only_rel is None:
            self._pending_graph = self.graph.contract(repo) if self.graph is not None else None
            if file_churn:
                self._pending_churn = INPUTS.write("lucidlint-churn-", json.dumps(dict(file_churn)))
            if coverage is not None and coverage.result.lines is not None:
                # the repo's OWN snapshot is the coverage truth (R6/D6): the
                # core reads which files it names and which lines it saw
                # executed. A file it does not name is UNKNOWN to the core.
                request = CoverageRequest(
                    source=coverage.result.source,
                    stale=coverage.stale,
                    covered={rel: sorted(lines) for rel, lines in coverage.result.lines.items()},
                )
                self._pending_coverage = INPUTS.write("lucidlint-coverage-", request.to_json())
            self._pending_docs = str(repo)
            self._pending_gitignored = gitignored_docs(repo, self._host)

    def _flags(self) -> ScanFlags:
        return ScanFlags(
            self._pending_graph,
            self._pending_churn,
            self._pending_coverage,
            self._pending_tests,
            self._pending_docs,
            self._pending_gitignored,
        )

    def active(self, repo: Path) -> bool:
        """True when the Rust core is available (it is required)."""
        return self.binary(repo) is not None

    @staticmethod
    def _scanner_candidates(repo: Path, exe: str) -> list[Path]:
        """The binary locations tried in order: the CODE_HEALTH_SCANNER env var,
        the repo-local build, the tool-checkout build, then the release bundle.
        `exe` carries the platform suffix (.exe on Windows) — every candidate
        must use it, or a Windows build is silently skipped."""
        candidates: list[Path] = []
        env = os.environ.get("CODE_HEALTH_SCANNER")
        if env:
            candidates.append(Path(env))
        candidates.append(repo / "scanner" / "target" / "release" / f"lucidlint{exe}")
        candidates.append(artifact_root() / "scanner" / "target" / "release" / f"lucidlint{exe}")
        bundle_dir = artifact_root() / "bin"
        candidates.append(bundle_dir / f"lucidlint{exe}")
        # a pip install ships the scan core as package-data at
        # lucidlint_bin/bin/lucidlint (setup.py builds it into the wheel) — the
        # self-contained pip channel, sibling of the module like the bundle bin
        candidates.append(artifact_root() / "lucidlint_bin" / "bin" / f"lucidlint{exe}")
        return candidates

    @staticmethod
    def _scanner_build_is_stale(binary: Path) -> bool:
        """True when the binary is a LOCAL cargo build (.../scanner/target/
        release/<bin>) whose scanner sources are newer — a stale core would
        silently test old rules. False for env-pinned and bundled binaries:
        they are deliberate pins, and a dev checkout's sources are legitimately
        newer than a released wheel. The source root is structural — the
        `scanner` dir two levels above the binary — so this is a pure
        path/mtime decision, testable without make. The walk covers `src/`
        plus the manifests only: `target/` (build artifacts, gigabytes) never
        decides staleness — cargo owns rebuild correctness there."""
        if len(binary.parents) < 3 or binary.parents[2].name != "scanner":
            return False
        newest = 0.0
        src_root = binary.parents[2]
        watched = [src_root / "src", src_root / "Cargo.toml", src_root / "Cargo.lock"]
        for root in watched:
            if root.is_dir():
                for p in root.rglob("*"):
                    if p.is_file() and p.suffix == ".rs":
                        newest = max(newest, p.stat().st_mtime)
            elif root.is_file():
                newest = max(newest, root.stat().st_mtime)
        return newest > binary.stat().st_mtime

    @staticmethod
    def _rust_finding_rel(file_val: str, repo: Path, rels: set[str]) -> str | None:
        """The binary reports per-file findings with the path as passed and the
        repo-wide ones (duplicate/unused) with the repo-relative path — normalize.
        A directory-level finding (`flat-folder`, `folder-mix`) names a directory
        or `.`; both are accepted when the path resolves inside the repo."""
        if file_val in rels:
            return file_val
        if file_val in ("", "."):
            return "."
        try:
            # resolve BOTH sides: with --repo . (relative), a relative base makes
            # relative_to raise and the finding would be silently dropped. A
            # relative finding path is repo-relative (the binary runs at the repo
            # root), so join it to the repo — resolving it against THIS process's
            # cwd loses every directory-level finding (flat-folder, folder-mix)
            p = Path(file_val)
            if not p.is_absolute():
                p = repo / p
            rel = p.resolve().relative_to(repo.resolve()).as_posix()
            # is for a file outside this scan set — drop it, not a failure to surface
        except (ValueError, OSError):  # lucidlint: ignore swallow an unmappable path means the finding
            rel = ""
        if RustScan._is_directory_finding(repo, rel, rels):
            return rel  # a directory-level finding (flat-folder, folder-mix)
        return rel if rel in rels else None

    @staticmethod
    def _is_directory_finding(repo: Path, rel: str, rels: set[str]) -> bool:
        """The finding names a directory rather than a scanned file — flat-folder
        and folder-mix report at folder level."""
        return bool(rel) and rel not in rels and (repo / rel).is_dir()

    @staticmethod
    def _unmapped_note(values: set[str]) -> str:
        """The diagnostic for findings whose file cannot be mapped into the scan
        set. This used to be a silent `continue`, which hid every repo-wide family
        for single-package layouts (the inferred root lost the package segment);
        a finding file that cannot be mapped is a root-resolution mismatch, not a
        clean run."""
        shown = ", ".join(sorted(values)[:5])
        more = "" if len(values) <= 5 else f" (+{len(values) - 5} more)"
        return (
            f"dropped findings for {len(values)} file(s) outside the scan set: {shown}{more} — "
            "a finding file that cannot be mapped back is a root-resolution mismatch, not a clean run"
        )

    @staticmethod
    def _actions_from_rust(
        rust: RustFindings, include_tests: bool, file_churn: Counter[str], last_modified: dict[str, str]
    ) -> list[Action]:
        """The contract findings ARE the action model — converted verbatim, with
        the orchestrator's fields (churn, provenance, raw risk) filled in."""
        actions: list[Action] = []
        for rel, findings in rust.by_rel.items():
            if not include_tests and SourceWalk.is_test_path(rel):
                # test files are excluded from the health scan, but the rules
                # that live in tests are scanned for alone
                findings = [f for f in findings if f.signal in RustScan.TEST_ONLY_SIGNALS]
            for f in findings:
                churn = file_churn.get(rel, 0)
                actions.append(
                    Action(
                        kind=f.kind,
                        severity=f.severity,
                        file=rel,
                        line=f.line,
                        col=f.col,
                        function=f.function,
                        message=f.message,
                        metric=f.metric,
                        signal=f.signal,
                        churn=churn,
                        last_modified=last_modified.get(rel, ""),
                        tested="",
                        raw=FileHistory.raw_score(f.kind, f.metric or 1, churn),
                        seam_members=f.seam_members,
                        fix_kind=f.fix_kind,
                    )
                )
        return actions

    def scan(
        self,
        repo: Path,
        args,
        file_churn: Counter[str],
        only_rel: str | None = None,
        coverage: CoverageInput | None = None,
    ) -> RustFindings:
        """Every finding family computes in the Rust core (per-file, partition,
        test rules, duplicate/unused, record-shape, complexity, the graph
        families, hotspot, abstraction, docs). The thresholds live in the binary
        (schema 4); the report header and suppression census ride on the result
        for the banner + footer ledger. `coverage` is the repo's own snapshot —
        the core reads which files it names and which lines it saw executed, so a
        file the snapshot omits stays UNKNOWN (R6)."""
        if not self.active(repo):
            # no Python fallback — the binary is required; a silent empty scan
            # would report GATE: PASS without checking anything (fail-fast)
            raise RuntimeError(
                "the scan binary is required — build it with `make scanner-check` "
                "or install the lucidlint release bundle"
            )
        self.prepare(repo, only_rel, args.include_tests, file_churn, coverage)
        files = SourceWalk(repo, self._host).py_files(only_rel)
        rust = self.load(repo, files)
        if rust is None:
            raise RuntimeError("the Rust scan core failed — rebuild with `make scanner-check`")
        return rust


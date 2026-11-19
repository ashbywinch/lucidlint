"""The `lucidlint fix` command: one command's whole behaviour.

R27 line resolution, the engine checks, the preview surface, and the apply
path. A fix targets one file, so `FixTarget` owns the file's own questions
(which lines carry a finding, what a single-file scan says, how a Rust fix
runs), and `FixCommand` owns the request: the kind, the anchor, the name, the
preview, and the write.
"""

from __future__ import annotations

import argparse
import difflib
import re
import sys
from collections import Counter
from pathlib import Path
from subprocess import SubprocessError
from types import ModuleType
from typing import NamedTuple

import rule_metadata
from orchestrator.console import CONSOLE
from orchestrator.host import HOST
from orchestrator.rust_fix_request import RustFixRequest
from orchestrator.services import SERVICES
from orchestrator.source_walk import SourceWalk


class FixTarget(NamedTuple):
    """The file a fix targets — its repo and repo-relative path. The fixers
    are methods on it: every fixer needs the same (repo, rel) pair (the
    strewing pattern: functions sharing a domain class are its methods)."""

    repo: Path
    rel: str

    def fix_rust(self, kind: str, line: int, name: str | None) -> int:
        """A Rust fix runs in the scan core (syn), not the Python/libcst
        engine: invoke the binary's --fix mode, which edits in place."""
        repo, rel = self.repo, self.rel
        binary = SERVICES.rust_scan.binary(repo)
        if binary is None:
            print("fix: the Rust scan core is required — build it with `make scanner-check`")
            return 1
        target = repo / rel
        before = target.read_text(encoding="utf-8") if target.exists() else None
        if kind in FixCommand.name_required_kinds() and not (name or "").strip():
            # the scanner would get "" and either write `fn ()` or fail
            # with "nothing to change" — both lie about a missing
            # prerequisite. Refuse with the naming guidance instead.
            print(FixCommand.refusal(kind, None, None, rel, line))
            return 0
        spec = RustFixRequest(kind=kind, file=str(target), line=line, name=name or "").to_json()
        try:
            proc = HOST.subprocess.run(
                [str(binary), "--fix", spec], capture_output=True, text=True, timeout=120, cwd=str(repo)
            )
        except SubprocessError:
            print(f"fix: the Rust fix core failed for {rel}:{line}")
            return 1
        if proc.returncode != 0:
            # the scanner exits non-zero on an unknown/malformed request — an
            # unexpected error must surface (R29), not masquerade as a success
            if proc.stderr:
                sys.stderr.write(proc.stderr)
            return 1
        if before is not None:
            CONSOLE.print_diff(before, target.read_text(encoding="utf-8"), rel)
        return 0

    def finding_lines(self, kind: str) -> list[int]:
        """The lines of every finding of `kind` in this file — the R27 line
        resolution: the tool scans, the agent never counts lines.

        `kind` is the FIX kind (the directive's `fix: <kind>` tail — what the
        agent's command names): a dispatch-shaped complexity finding carries
        the kind `complexity` but its message directive says
        `fix: dispatch-registry`, so the match is against the message's
        directive, not the raw kind."""
        wanted = FixCommand.ALIASES.get(kind, kind)
        found = []
        for f in self.scan_single_file():
            directive = FixCommand.directive_kind(f.message)
            if directive is None:
                # a finding with no directive has no fix — it never matches
                continue
            if FixCommand.ALIASES.get(directive, directive) == wanted:
                found.append(f.line)
        # two operands on one line (magic-number twins) are one ANCHOR —
        # re-attachment must not see "live findings at 2, 2"
        return sorted(set(found))

    def scan_single_file(self):
        """Scan this file, yielding its findings."""
        repo, rel = self.repo, self.rel
        let_include_tests = False  # the fix resolves one file's findings
        SERVICES.rust_scan.prepare(repo, rel, let_include_tests, Counter())
        files = SourceWalk(repo).py_files(rel)
        rust = SERVICES.rust_scan.load(repo, files)
        if rust is None:
            return
        yield from rust.by_rel.get(rel, [])


class FixCommand:
    """The `lucidlint fix` command: R27 line resolution, the engine checks,
    the preview surface, and the apply path — one command's behavior, not a
    slice of main()."""

    @staticmethod
    def trim_preview_diff(diff, name, new_source):
        """Shape the extract-method preview: the call-site hunk (header + a few
        context lines + the inserted call + the next context line — the removed
        lines duplicate the extracted body) plus the extracted method's first
        lines. The agent names and comprehends from the method being created."""
        hunks, cur = [], []
        for line in diff:
            if line.startswith("@@"):
                if cur:
                    hunks.append(cur)
                cur = [line]
            else:
                cur.append(line)
        if cur:
            hunks.append(cur)
        call_hunk = next(
            (h for h in hunks if any(ln.startswith("+") and f"{name}(" in ln for ln in h)),
            hunks[0] if hunks else [],
        )
        if call_hunk:
            call_idx = next(i for i, ln in enumerate(call_hunk) if ln.startswith("+") and f"{name}(" in ln)
            head = [ln for ln in call_hunk[1:call_idx] if ln.startswith(" ")][:3]
            tail = next((ln for ln in call_hunk[call_idx + 1 :] if ln.startswith(" ")), None)
            shown = [call_hunk[0]] + head + [call_hunk[call_idx]]
            if tail:
                shown.append(tail)
            omitted = len(call_hunk) - len(shown)
            if omitted > 0:
                shown.append(f"... ({omitted} diff lines omitted)")
            call_hunk = shown
        src_lines = new_source.splitlines()
        def_idx = next(
            (i for i, ln in enumerate(src_lines) if ln.startswith("def ") and name in ln),
            None,
        )
        body: list[str] = []
        if def_idx is not None:
            for ln in src_lines[def_idx + 1 :]:
                if ln and not ln[0].isspace():
                    break
                body.append(ln)
        return call_hunk, def_idx, body
    @staticmethod
    def name_required_kinds() -> set[str]:
        """The engine owns the name-required policy. With libcst absent nothing
        can APPLY anyway, so an empty set only degrades refusal wording, never
        gating (the engine-present paths always see the real set)."""
        return set(HOST.fix_engine._NAME_REQUIRED_KINDS) if HOST.fix_engine.get() else set()
    @staticmethod
    def name_is_invalid(name: str | None) -> bool:
        """A --name that is not a Python identifier cannot be built into a fixer's
        node; a missing name is fine (previews run nameless)."""
        return name is not None and not name.isidentifier()
    @staticmethod
    def identifier_problem(kind: str, name: str | None, params: list[str] | None, where: str) -> str | None:
        """The invalid-argument guard: a non-identifier --name or --params entry
        would construct an invalid libcst node inside a fixer. Refused before
        dispatch with a message; a MISSING name is NOT refused here — previews
        run nameless by contract."""
        if kind in FixCommand.name_required_kinds() and FixCommand.name_is_invalid(name):
            return (
                f"fix: --name must be a valid Python identifier, got '{name}' "
                f"at {where} - replace the message's <placeholder> with the real name"
            )
        if params is not None:
            bad = [p for p in params if not p.isidentifier()]
            if bad:
                return f"fix: --params entries must be valid identifiers, got {bad} at {where}"
        return None
    @staticmethod
    def refusal(kind: str, name: str | None, params: list[str] | None, file: str, line: int) -> str:
        """Why a fix produced nothing. A name-required kind with a missing or
        non-identifier name is NOT 'nothing to change' — it is an unsatisfied
        prerequisite, and the silence would read as the finding being
        unfixable (the LSP placeholder flow depends on this message)."""
        if kind in FixCommand.name_required_kinds() and name is None:
            # an unsatisfied prerequisite, not 'nothing to change': the silence
            # would read as unfixable (the LSP placeholder flow depends on this)
            return (
                f"fix: {kind} needs a semantic name the tool cannot invent "
                f"(--name <Name>) at {file}:{line} - name it with a domain noun; naming is the judgement call"
            )
        return FixCommand.identifier_problem(kind, name, params, f"{file}:{line}") or (
            f"fix: nothing to change for {kind} at {file}:{line}"
        )
    @staticmethod
    def directive_kind(message: str) -> str | None:
        """The fix kind a finding's directive announces — what the agent's
        `--kind` names. The directive tail is the full command
        (`— fix: lucidlint fix --kind <kind> --file F --line N ...`); the
        pre-command form (`— fix: <kind> <prose>`) appears on message paths
        that have not been rewritten. None when the finding has no directive
        (R28: no fix exists — it never matches a fix request)."""
        idx = message.rfind("— fix: ")
        if idx == -1:
            return None
        tail = message[idx + len("— fix: ") :]
        m = re.search(r"--kind\s+([a-z-]+)", tail)
        if m:
            return m.group(1)
        first = tail.split()[0].strip()
        if first and first != "lucidlint":
            return first
        return None

    # the display kind -> the fix kind the command accepts: the gate reports
    # DISPLAY kinds (final_kind output: strewing shows as latent-class); the
    # fix surfaces accept either and normalize here
    ALIASES = {"latent-class": "extract-class", "complexity": "extract-method", "large-function": "extract-method"}

    def __init__(self, args, repo: Path):
        self.args: argparse.Namespace = args
        self.repo: Path = repo
        self.rel: str = args.file or ""
        self.fix_kind: str = ""

    def _params(self) -> list[str] | None:
        return self.args.params.split(",") if self.args.params else None

    def run(self) -> int:
        # the agent-driven fix surface: `lucidlint fix --kind X --file F --line N`
        if not self.rel.endswith((".py", ".rs")):
            # the fix engines (libcst for Python, syn for Rust) cannot touch a
            # non-Python/Rust target; refuse clearly instead of a parse crash
            print(
                f"fix: the fix engine only supports Python and Rust files — '{self.rel}' "
                f"is neither; refactor it by hand"
            )
            return 1
        self.fix_kind = FixCommand.ALIASES.get(self.args.kind, self.args.kind)
        # validate BEFORE dispatch: an INVALID identifier (<CONST>) reaching a
        # fixer constructs an invalid libcst Name and dies with a traceback,
        # not a message (the LSP flow hands the verbatim tokens to a shell).
        # A MISSING name still previews — the preview IS the line-number-free
        # contract; only the apply needs the commitment.
        bad = FixCommand.identifier_problem(
            self.fix_kind, self.args.name, self._params(), f"{self.args.file}:{self.args.line}"
        )
        if bad:
            print(bad)
            return 1
        if self.args.line == 0:
            exit_code = self._resolve_line()
            if exit_code is not None:
                return exit_code
        if self.rel.endswith(".rs"):
            # a Rust fix runs in the scan core (syn) — libcst is Python-only
            if self.fix_kind == "extract-method" and self.args.name is None and not self.args.confirm:
                # H4: extract-method's semantic name is REQUIRED. A name-less
                # request forwarded to the scanner would serialize "" and write
                # `fn ()` — invalid Rust — so refuse silently (R28; the
                # directive prose already told the agent to pass --name). There
                # is no Rust preview surface yet.
                print(FixCommand.refusal(self.fix_kind, self.args.name, self._params(), self.args.file, self.args.line))
                return 0
            return FixTarget(self.repo, self.rel).fix_rust(self.fix_kind, self.args.line, self.args.name)
        # Python: the libcst fix engine is a mandatory dependency
        fe = HOST.fix_engine.get()
        if fe is None:
            # the release bundle vendors libcst in deps/ when it can (the
            # static musl bundle cannot); a wheel install always declares it
            print(
                "fix: the Python fix engine requires libcst and none is installed here — "
                "install lucidlint from the pip wheel (libcst is a declared dependency), "
                "or run `uv sync` in a dev checkout"
            )
            return 1
        # schema-3 anchor: same-line twins need the finding's column — the
        # innermost match wins, mirroring the peel binding order. The
        # fix-kind to finding-kind mapping differs: extract-record-class is
        # the fix for record-shape findings; magic-number is its own kind.
        col = 0
        signal = "record-shape" if self.fix_kind == "extract-record-class" else self.fix_kind
        if signal:
            anchors = [
                f.col
                for f in FixTarget(self.repo, self.rel).scan_single_file()
                if f.signal == signal and f.line == self.args.line and f.col
            ]
            col = max(anchors) if anchors else 0
        if self.fix_kind not in fe.MECHANICAL_KINDS and self.fix_kind not in fe.STRUCTURAL_KINDS:
            # R31 (docs/PRD.md): messages address the actual mistake in
            # plain language — never a bare "no auto-fix exists"
            if self.fix_kind in rule_metadata.CATALOG.kinds():
                print(
                    f"fix: '{self.args.kind}' cannot be fixed by this command — "
                    f"the report line for it says what to change"
                )
            else:
                near = difflib.get_close_matches(self.fix_kind, fe.FIXABLE_KINDS, n=1)
                hint = (
                    f" Did you mean '{near[0]}'?"
                    if near
                    else " Run the scan — its report lines name the checks this command can fix."
                )
                print(f"fix: '{self.args.kind}' is not a fixable kind.{hint}")
            return 1
        req = fe._FixRequest(
            kind=self.args.kind,
            repo=self.repo,
            rel=self.rel,
            line=self.args.line,
            opts=fe.FixOptions(params=self._params(), name=self.args.name, callee=self.args.callee),
            col=col,
        )
        if self._previews_only(fe):
            return self._preview(req)
        return self._apply(req)

    def _previews_only(self, engine: ModuleType) -> bool:
        """A fix kind that only previews, with no name or --confirm, stops at
        the preview — the caller never meant to write."""
        return self.fix_kind in engine.PREVIEW_KINDS and not self.args.name and not self.args.confirm

    def _resolve_line(self) -> int | None:
        """R27: agents never compute line numbers — the tool owns its own
        coordinates; when the file has exactly one finding of the kind, no
        --line is needed. Returns an exit code when the request cannot
        proceed, None when the line is resolved."""
        # R27: agents never compute line numbers — the tool owns its own
        # coordinates; when the file has exactly one finding of the kind,
        # no --line is needed (both the .py and .rs fix surfaces)
        lines = FixTarget(self.repo, self.args.file).finding_lines(self.args.kind)
        if len(lines) == 1:
            self.args.line = lines[0]
        elif not lines:
            return 0  # nothing of this kind exists — R28 silence
        else:
            print(
                f"fix: {len(lines)} {self.args.kind} findings in {self.args.file} "
                f"(lines {', '.join(map(str, lines))}) — pass --line to pick one"
            )
            return 0
        return None

    def _preview(self, req) -> int:
        # the name-free preview surface: show the proposed refactoring
        # as a diff (the seam with a placeholder name — no --fix-name
        # needed to see it). The agent reviews the seam, then re-runs
        # with --fix-name <name>; the name is the commitment, so the
        # named run applies — no --confirm dance
        new_source, description = req.propose_finding()
        if new_source is None:
            # no seam exists — the finding is unfixable HERE; the missing-name
            # refusal would send the agent on a naming chase that cannot
            # succeed (the name-required gate lives in the apply path, where
            # a seam EXISTS) (review bot)
            return 0  # no seam — R28 silence
        diff = list(
            difflib.unified_diff(
                (self.repo / self.args.file).read_text().splitlines(),
                new_source.splitlines(),
                fromfile=self.args.file,
                tofile=self.args.file + " (proposed)",
                lineterm="",
            )
        )
        if self.fix_kind == "extract-method":
            # the C shape (subtask-tested): the call-site hunk plus the
            # EXTRACTED METHOD's first lines — agents name and comprehend
            # from the method being created, not the diff head; a bare
            # diff-head truncation left them wanting the cut part and
            # unsure whether _extracted was the final name
            call_hunk, def_idx, body = FixCommand.trim_preview_diff(diff, self.args.name or "_extracted", new_source)
            print("\n".join(diff[:2]))  # --- / +++ file headers
            print("\n".join(call_hunk))
            print(f"# seam: {description}")
            if def_idx is not None:
                print("\nExtracted (the method being created, first lines):\n")
                print(new_source.splitlines()[def_idx])
                print("\n".join(body[:10]))
                if len(body) > 10:
                    print(f"... ({len(body) - 10} more lines omitted)")
        else:
            if len(diff) > 40:
                diff = diff[:40] + [f"... ({len(diff) - 40} more lines omitted)"]
            print("\n".join(diff))
        print(
            f"# the name `{self.args.name or '_extracted'}` is a placeholder — pick a real domain noun; "
            f"apply it: lucidlint fix --kind {self.args.kind} --file {self.args.file} "
            f"--line {self.args.line} --name <name>"
        )
        return 0

    def _apply(self, req, moved: bool = False) -> int:
        target = self.repo / self.args.file
        before = target.read_text(encoding="utf-8") if target.exists() else None
        description = req.fix_finding()
        if description is None:
            if req.decline:
                # autofix pending agent-supplied input, at a good anchor
                print(f"fix: {self.fix_kind} at {self.args.file}:{req.line} needs input — {req.decline}")
                return 0
            if self.fix_kind in FixCommand.name_required_kinds() and self.args.name is None:
                print(
                    f"fix: {self.fix_kind} at {self.args.file}:{req.line} needs a semantic name "
                    f"the tool cannot invent — pass --name <Name> (name it with a domain noun)"
                )
                return 0
            return self._reattach_or_silence(req, moved)
        print(f"fix: applied {self.fix_kind} at {self.args.file}:{req.line} — {description}")
        if before is not None:
            # extract-module moves defs into a NEW module: the diff here is
            # the origin file; the created module's content is the preview
            # the agent already reviewed
            CONSOLE.print_diff(before, target.read_text(encoding="utf-8"), self.args.file)
        if req.extra_writes or req.deletes:
            self._verify_multi_file(req)
        return 0

    def _reattach_or_silence(self, req, moved: bool) -> int:
        """The anchor no longer matches a writing fix. R27: the tool owns its
        coordinates — re-attach to the finding's current line when that is
        unambiguous (announcing the move), ask when it is not, and stay
        silent when nothing of this kind remains."""
        if moved:
            return 0  # already re-attached once and it still will not write
        lines = [ln for ln in FixTarget(self.repo, self.args.file).finding_lines(self.fix_kind) if ln != self.args.line]
        if len(lines) == 1:
            print(f"fix: anchor moved — {self.fix_kind} now at {self.args.file}:{lines[0]}")
            req.line = lines[0]
            req.source = None
            req.decline = None
            # the reattached line's anchor column — same-line twins need the
            # finding's col just like the direct path (schema-3)
            anchors = [
                f.col
                for f in FixTarget(self.repo, self.args.file).scan_single_file()
                if f.signal == (self.fix_kind if self.fix_kind != "extract-record-class" else "record-shape")
                and f.line == req.line
                and f.col
            ]
            req.col = max(anchors) if anchors else 0
            return self._apply(req, moved=True)
        if len(lines) > 1:
            print(
                f"fix: {self.fix_kind} anchor moved — live findings at "
                f"{', '.join(map(str, lines))} — pass --line to pick one"
            )
        return 0  # nothing of this kind remains — R28 silence

    def _verify_multi_file(self, req) -> None:
        """R8: a multi-file fix is verified by the REPO-WIDE scan — the
        finding kind must be gone (scan_single_file cannot see a chain or a
        split's cross-file layout). A scan failure is NOT flipped into a
        fix failure — the apply already succeeded."""
        try:
            rust = SERVICES.rust_scan.scan(self.repo, self.args, Counter(), None)
        except Exception:
            return
        remaining = [
            f
            for fs in rust.by_rel.values()
            for f in fs
            if f.signal in {"forwarding-chain", "class-module", "delegating-husk"}
        ]
        if remaining:
            rels = sorted({f.file for f in remaining})[:3]
            print(
                f"fix: {self.fix_kind} applied at {self.args.file}:{req.line} — re-scan still finds "
                f"{len(remaining)} related finding(s) in {', '.join(rels)}{'…' if len(rels) > 3 else ''}: "
                "iterate the fix"
            )

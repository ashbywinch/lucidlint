"""The repo-scan gate flow.

One run's pipeline state (history, coverage, actions, baselines) lives on the
runner instead of threading through the entry as parameters — the shape the
assembly-class rule flags as a class in waiting. The verdict (stale baseline
entries and fail actions both block) is the runner's own too.
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import time
import xml.etree.ElementTree as ET
from collections import Counter
from pathlib import Path

from orchestrator.action import Action
from orchestrator.baseline import Baseline
from orchestrator.console import CONSOLE
from orchestrator.host import Host
from orchestrator.lucidlint_config import LucidlintConfig
from orchestrator.report.render_ctx import RenderCtx
from orchestrator.rust_scan import RustScan
from orchestrator.services import Services
from orchestrator.sources.coverage_context import CoverageContext, coverage_context
from orchestrator.sources.coverage_input import CoverageInput
from orchestrator.sources.coverage_result import CoverageResult
from orchestrator.sources.file_history import FileHistory
from orchestrator.sources.git_head import GitHead


# lucidlint: ignore-file record-shape the libcst layer's tuple/dict shorthands ARE its wire records —
# lucidlint: ignore-file record-shape a class per helper hop is ceremony
# the domain's
# lucidlint: ignore process-class _GateRunner is the CLI's one pipeline object — its state is real and the name is
class GateRunner:
    # a churn window is measured in days; git's clock is seconds
    _SECONDS_PER_DAY = 86400
    # the priority column spreads raw risk over a 1-99 band so the list reads apart
    _PERCENTILE_LOW = 1
    _PERCENTILE_HIGH = 99

    """The repo-scan gate flow. The pipeline state (history, coverage,
    actions, baselines) lives on the runner instead of threading through
    main() as parameters — the shape the assembly-class rule flags as a
    class in waiting."""

    def __init__(self, repo: Path, args: argparse.Namespace, host: Host, services: Services):
        self.repo: Path = repo
        self.args: argparse.Namespace = args
        self._host: Host = host
        self.services: Services = services
        self.fh: FileHistory | None = None
        self.cr: CoverageResult | None = None
        self.cc: CoverageContext | None = None
        self.actions: list[Action] = []
        self.report_header: str = ""
        self.suppression_census: dict[str, int] = {}
        self.ignored_config: LucidlintConfig = LucidlintConfig(set(), [])
        self.ignored_by_signal: Counter[str] = Counter()
        # B5: signal -> the config key that ignores it (per-path glob or the
        # repo-wide list) — per-item suppression advice must name it
        self.config_ignore_keys: dict[str, str] = {}
        # B3: the one-line pre-round baseline migration note ("" = none)
        self.baseline_migration: str = ""
        self.unique: list[Action] = []
        self.stale: list[str] = []
        self.baseline_ignored: dict[str, int] = {}
        self.head: GitHead | None = None
        self.rc: RenderCtx | None = None

    def gather(self) -> None:
        """History/coverage context (per-file mode skips the git work)."""
        if self.args.file:
            # per-file findings are what an editor shows on save.
            self.fh = FileHistory(Counter(), {})
            self.cr = CoverageResult(None, "")
            self.cc = coverage_context(self.repo, None, "")
            return
        if self.args.refresh_coverage:
            self._refresh_coverage()
        self.fh = self.file_history()
        self.cr = self.load_coverage()
        self.cc = coverage_context(self.repo, self.cr.lines, self.cr.source)

    def collect(self) -> None:
        """The finding actions for the repo, plus the scan core's report
        header and suppression census (the banner + footer ledger)."""
        fh = self.fh
        assert fh is not None, "gather() precedes collect"
        cc = self.cc
        assert cc is not None, "gather() precedes collect"
        cr = self.cr
        assert cr is not None, "gather() precedes collect"
        rust = self.services.rust_scan.scan(
            self.repo,
            self.args,
            fh.churn,
            only_rel=self.args.file,
            coverage=CoverageInput(cr, cc.graph_preferred),
        )
        self.actions = RustScan._actions_from_rust(rust, self.args.include_tests, fh.churn, fh.last_modified)
        self.report_header = rust.header
        self.suppression_census = rust.suppressions or {}

    def apply_config(self) -> None:
        """Project-wide config: [lucidlint.guidance] appends the house rule to
        every matching finding's message — one reviewed config line replaces N
        per-site citations and travels with each future finding; global and
        per-path ignores filter BEFORE the verdict, COUNTING what they hide —
        the §9 debt ledger, so an ignore cannot grow invisibly ("nothing is
        ever wrong")."""
        self.ignored_config = self._load_lucidlint_config()
        self.ignored_by_signal = Counter()
        # B5: remember which config key ignores each signal, so a finding's
        # per-item advisory can name it instead of offering a bare site marker
        self.config_ignore_keys = {}
        for sig in self.ignored_config.global_ignore:
            self.config_ignore_keys.setdefault(sig, "the repo-wide ignore list")
        for pattern, path_ignored in self.ignored_config.per_path_ignore:
            for sig in path_ignored:
                self.config_ignore_keys.setdefault(sig, f"'{pattern}'")
        for a in self.actions:
            house_rule = self.ignored_config.guidance.get(a.signal)
            if house_rule is not None:
                a.message = f"{a.message} — house rule: {house_rule}"
        if not self.ignored_config.global_ignore and not self.ignored_config.per_path_ignore:
            return
        kept: list[Action] = []
        for a in self.actions:
            if a.signal in self.ignored_config.global_ignore:
                self.ignored_by_signal[a.signal] += 1
                continue
            removed = False
            for pattern, path_ignored in self.ignored_config.per_path_ignore:
                if Path(a.file).match(pattern) and a.signal in path_ignored:
                    self.ignored_by_signal[a.signal] += 1
                    removed = True
                    break
            if not removed:
                kept.append(a)
        self.actions = kept

    def file_history(self) -> FileHistory:
        """The recent git history: per-file change counts and last-modified date.

        The walk is bounded (the last 200 commits or 730 days, whichever stops
        first) — old churn dilutes the CURRENT hotspot signal, so limiting the
        window is more signal, not less. The walk is deterministic for a given
        HEAD, so the result is cached per (HEAD, window) — repeat gate runs skip
        the walk entirely. `last_modified` is the NEWEST touch (the walk is
        newest-first; first-seen-wins).
        """
        if self._host.pygit2 is None or not (self.repo / ".git").exists():
            # no pygit2 or not a git self.repo — the absence is certain and nothing
            # can fix it in this run: silent (never announce an unfixable gap)
            return FileHistory(Counter(), {})
        head = ""
        cache_key = ""
        try:
            r = self._host.pygit2.Repository(str(self.repo))
            head = str(r.head.target)
            cutoff = int(time.time()) - FileHistory.MAX_AGE_DAYS * GateRunner._SECONDS_PER_DAY
            cutoff_day = time.strftime("%Y-%m-%d", time.localtime(cutoff))
            cache_key = f"churn-{head}-{FileHistory.MAX_COMMITS}-{cutoff_day}.json"
            cache_path = self.repo / ".lucidlint-cache" / cache_key
            try:
                data = json.loads(cache_path.read_text())
                return FileHistory(Counter(data["churn"]), data["last_modified"])
            except (OSError, ValueError, KeyError):  # lucidlint: ignore swallow a missing/corrupt cache just walks
                pass  # no cache yet — walk
        except KeyError:
            return FileHistory(Counter(), {})  # not a git self.repo — certain: silent
        except Exception as e:  # lucidlint: ignore broad-except pygit2's error types vary by version; the walk degrades and the warning names the cause
            CONSOLE.warn(f"churn: {e}")  # unexpected — show the actual error
            return FileHistory(Counter(), {})

        churn: Counter[str] = Counter()
        last: dict[str, str] = {}
        try:
            r = self._host.pygit2.Repository(str(self.repo))
            cutoff = int(time.time()) - FileHistory.MAX_AGE_DAYS * GateRunner._SECONDS_PER_DAY
            for seen, commit in enumerate(r.walk(r.head.target, self._host.pygit2.enums.SortMode.TIME)):
                if seen >= FileHistory.MAX_COMMITS or commit.commit_time < cutoff:
                    break
                date = str(commit.commit_time)
                changed = set()
                if commit.parents:
                    diff = commit.tree.diff_to_tree(commit.parents[0].tree)
                    for patch in diff:
                        delta = patch.delta if patch is not None else None
                        path = delta.new_file.path if delta is not None else ""
                        if FileHistory.tracked_churn_path(path):
                            changed.add(path)
                else:
                    # initial commit — diff against the empty tree gives every file
                    for patch in commit.tree.diff_to_tree():
                        delta = patch.delta if patch is not None else None
                        path = delta.new_file.path if delta is not None else ""
                        if FileHistory.tracked_churn_path(path):
                            changed.add(path)
                for path in changed:
                    churn[path] += 1
                    if date and path not in last:
                        last[path] = date  # first-seen = newest in a newest-first walk
        except Exception as e:  # lucidlint: ignore broad-except pygit2's diff API fails per version; the partial history is surfaced with the cause
            CONSOLE.warn(f"churn walk: {e}")  # unexpected — show the actual error
            return FileHistory(churn, last)  # partial/empty history, surfaced

        if cache_key:
            try:
                (self.repo / ".lucidlint-cache").mkdir(exist_ok=True)
                (self.repo / ".lucidlint-cache" / cache_key).write_text(
                    # persisted wire format — round-trips verbatim across runs
                    json.dumps({"churn": dict(churn), "last_modified": last})
                )
            except OSError:  # lucidlint: ignore swallow the cache is best-effort — a read-only self.repo still works
                pass  # the cache is best-effort — a read-only self.repo still works
        return FileHistory(churn, last)

    def load_coverage(self) -> CoverageResult:
        """Per-file covered line sets, preferring the self.repo's own coverage data.

        Sources, in order: coverage.xml (Cobertura, what CI gates on), then
        .coverage (coverage.py SQLite, line_bits format). The graph's TESTED_BY
        edges miss tests that import inside the test body — real coverage data
        does not. lines is None when neither source exists.
        """
        if (self.repo / "coverage.xml").exists():
            return self._coverage_from_xml()
        if (self.repo / ".coverage").exists():
            return self._coverage_from_sqlite()
        return CoverageResult(None, "no coverage data (no coverage.xml, no .coverage)")

    def _coverage_from_xml(self) -> CoverageResult:
        """Cobertura coverage.xml: class line elements with hits > 0."""
        try:
            root = ET.parse(self.repo / "coverage.xml").getroot()
        except ET.ParseError:
            return CoverageResult(None, "coverage.xml unparseable")
        covered: dict[str, set[int]] = {}
        for cls in root.iter("class"):
            filename = (cls.get("filename") or "").replace("\\", "/")
            if not filename.endswith(".py"):
                continue
            lines = covered.setdefault(self.rel_path(filename), set())
            for ln in cls.iter("line"):
                if int(ln.get("hits", "0") or 0) <= 0:
                    continue
                raw_number = ln.get("number")
                if raw_number is None:
                    CONSOLE.warn(f"ignoring malformed <line> element in {self.repo / 'coverage.xml'}")
                    continue
                try:
                    lines.add(int(raw_number))
                except ValueError:  # lucidlint: ignore swallow malformed <line> elements are skipped
                    CONSOLE.warn(f"ignoring malformed <line> element in {self.repo / 'coverage.xml'}")
        return CoverageResult(covered or None, "coverage.xml")

    def _coverage_from_sqlite(self) -> CoverageResult:
        """coverage.py .coverage SQLite: line_bits rows per file."""
        try:
            db = sqlite3.connect(self.repo / ".coverage")
            files = dict(db.execute("SELECT id, path FROM file"))
            covered: dict[str, set[int]] = {}
            for fid, numbits in db.execute("SELECT file_id, numbits FROM line_bits"):
                path = files.get(fid)
                if not path:
                    continue
                rel = self.rel_path(path)
                if not rel.endswith(".py"):
                    continue
                covered.setdefault(rel, set()).update(CoverageResult.numbits_to_lines(numbits))
            db.close()
            return CoverageResult(covered or None, ".coverage")
        except sqlite3.Error:
            return CoverageResult(None, ".coverage unreadable")

    def rel_path(self, p: str) -> str:
        """Graph stores absolute paths; radon/git use self.repo-relative. Normalize."""
        p = p.replace("\\", "/")
        root = str(self.repo.resolve()).replace("\\", "/") + "/"
        if p.startswith(root):
            p = p[len(root) :]
        return p

    def _refresh_coverage(self) -> None:
        """Run the self.repo's coverage suite so verdicts are fresh."""
        self._host.process.run(
            ["make", "-C", str(self.repo), "coverage"], capture_output=True, text=True, timeout=1800
        )

    def _load_lucidlint_config(self) -> LucidlintConfig:
        """The repo's project-wide config (cached per repo for the process)."""
        return LucidlintConfig.load(self.repo)

    def _git_head(self) -> GitHead:
        """Current branch and short commit for report provenance, via pygit2.
        Returns empty strings when git or pygit2 is unavailable."""
        if self._host.pygit2 is None:
            return GitHead(branch="", commit="")
        try:
            r = self._host.pygit2.Repository(str(self.repo))
            return GitHead(branch=r.head.shorthand or "", commit=str(r.head.target)[:7])
        # git-absent is a supported mode — the handler returns the empty head
        except Exception:  # lucidlint: ignore broad-except pygit2's absence and its errors are one case here: no head, branch named empty
            return GitHead(branch="", commit="")

    @staticmethod
    def _dedupe_merge(actions: list[Action]) -> list[Action]:
        """Dedupe, rank, merge per-target kinds, then lifecycle notes."""
        unique = GateRunner._dedupe(actions)
        GateRunner._percentile_rank(unique)
        unique = GateRunner._merge_targets(unique)
        GateRunner._percentile_rank(unique)
        GateRunner._lifecycle_notes(unique)
        return unique

    def _write_baseline(self, unique: list[Action], ignored_by_signal: Counter | None = None) -> int:
        """--update-baseline: lock all current action keys and exit clean."""
        if not self.args.baseline:
            CONSOLE.warn("--update-baseline requires --baseline PATH")
            return 2
        keys = [a.key() for a in unique if a.severity != "warn"]
        baseline: dict = {"actions": keys}
        if ignored_by_signal:
            baseline["config_ignored"] = dict(ignored_by_signal)
        self.args.baseline.write_text(json.dumps(baseline, indent=2))
        print(f"lucidlint: baseline written — {len(keys)} action(s) locked to {self.args.baseline}")
        return 0

    @staticmethod
    def _dedupe(actions: list[Action]) -> list[Action]:
        """Same kind+file+line+function+position fires once (graph and radon
        can both flag a function; record-shape's positions share a line)."""
        seen: dict[tuple, Action] = {}
        for a in actions:
            key = (a.kind, a.file, a.line, a.function, a.position_key())
            if key not in seen or a.raw > seen[key].raw:
                seen[key] = a
        return list(seen.values())

    @staticmethod
    def _percentile_rank(unique: list[Action]) -> None:
        """Rank raw risk 1-99 (percentile) so the list spreads."""
        if not unique:
            return
        lo, hi = min(a.raw for a in unique), max(a.raw for a in unique)
        band = GateRunner._PERCENTILE_HIGH - GateRunner._PERCENTILE_LOW
        for a in unique:
            a.priority = (
                GateRunner._PERCENTILE_HIGH
                if hi <= lo
                else max(
                    GateRunner._PERCENTILE_LOW,
                    round(GateRunner._PERCENTILE_LOW + band * (a.raw - lo) / (hi - lo)),
                )
            )

    @staticmethod
    def _merge_targets(unique: list[Action]) -> list[Action]:
        """Per-target merge: complexity + large-function on the same function is one fix."""
        merged: dict[tuple, Action] = {}
        for a in sorted(unique, key=lambda a: (-a.raw, a.file, a.line)):
            key = a.merge_key()
            if key not in merged:
                merged[key] = a
            else:
                prev = merged[key]
                prev.kinds = sorted({prev.kind, a.kind})
                prev.raw = max(prev.raw, a.raw)
                if a.note and a.note not in prev.note:
                    prev.note = (prev.note + " " + a.note).strip()
                prev.line = min(prev.line, a.line)
                if a.severity == "fail":
                    prev.severity = "fail"  # a warn merged into a fail target must not clear the gate
                if a.message != prev.message:
                    extra = f"{a.severity.upper()}: {a.message}"
                    if extra not in prev.note:
                        prev.note = (prev.note + " " + extra).strip()
        return list(merged.values())

    @staticmethod
    def _lifecycle_notes(unique: list[Action]) -> None:
        """Facts only — low-churn scripts/tools. Delete-vs-refactor is the agent's call."""
        for a in unique:
            low_churn_script = a.file.startswith(("scripts/", "tools/")) and a.churn <= 2 and bool(a.last_modified)
            if low_churn_script:
                a.note = (
                    a.note + f" Lifecycle: {a.churn}x churn, last touched {FileHistory.iso_day(a.last_modified)} — "
                    f"low-change file under scripts/tools."
                ).strip()

    def _render_feedback_request(self) -> None:
        """Append the closing ask to a full text report. The reader is the
        only source of a bad suggestion, so every full run ends by inviting
        one; per-file runs stay silent (the LSP reprints the report on the
        next edit)."""
        if self.args.file:
            return
        print()
        print(RenderCtx._FEEDBACK_REQUEST)

    def run(self) -> int:
        """The gate verdict for the repo."""
        self.gather()
        self.collect()
        self.apply_config()
        self.unique = self._dedupe_merge(self.actions)

        if self.args.update_baseline:
            return self._write_baseline(self.unique, self.ignored_by_signal)
        bl = Baseline(self.args.baseline)
        baseline_keys, self.baseline_ignored = bl.keys, bl.ignored
        self.stale = Baseline.apply(self.unique, baseline_keys)
        # B3: a pre-round 'actions' schema (line-less keys or action dicts)
        # cannot map to current identities — ONE migration line, never a
        # silent "+0 acknowledged"/"cannot tell what is new", never
        # phantom-stale failures. With a --baseline file the note names it;
        # without one, a repo-root lucidlint.json is recognized the same way.
        self.baseline_migration = ""
        if self.args.baseline and bl.legacy:
            self.baseline_migration = (
                f"{self.args.baseline.name} uses a pre-round 'actions' schema — re-acknowledge "
                f"with --update-baseline to migrate ({bl.legacy} actions ignored)"
            )
        elif self.args.baseline is None:
            _, legacy = Baseline.file_state(self.repo / "lucidlint.json")
            if legacy:
                self.baseline_migration = (
                    "lucidlint.json uses a pre-round 'actions' schema — re-acknowledge "
                    f"with --update-baseline to migrate ({legacy} actions ignored)"
                )
        # the §9 growth signal: a config-ignored family whose count GREW since
        # the baseline is debt being added to, not held — the ignore's scope is
        # wrong. A warning, never a gate failure (growing repos legitimately
        # grow): --update-baseline re-locks the new counts. Only meaningful when
        # a baseline exists to compare against.
        if self.args.baseline:
            for sig, n in self.ignored_by_signal.items():
                if self.baseline_ignored.get(sig, 0) < n:
                    CONSOLE.warn(
                        f"config-ignored '{sig}' grew {self.baseline_ignored.get(sig, 0)} -> {n} since the baseline — "
                        "the ignore's scope is being added to; re-scope it or re-lock with --update-baseline"
                    )
        fails = [a for a in self.unique if a.severity == "fail"]
        warns = [a for a in self.unique if a.severity == "warn"]
        acks = [a for a in self.unique if a.severity == "ack"]
        head = self._git_head()
        self.head = head
        cc = self.cc
        assert cc is not None, "gather() precedes render"
        self.rc = RenderCtx(
            self.repo,
            self.args,
            head.branch,
            head.commit,
            cc.label,
            cc.graph_preferred,
            ignored_by_signal=self.ignored_by_signal,
            report_header=self.report_header,
            suppression_census=self.suppression_census,
            config_ignore_keys=self.config_ignore_keys,
            baseline_migration=self.baseline_migration,
            stale_count=len(self.stale),
        )

        if self.args.json:
            # stdout is the JSON document — the migration note rides stderr
            if self.baseline_migration:
                CONSOLE.warn(self.baseline_migration)
            self.rc.render_json(self.unique)
        else:
            self.rc.render_text(self.unique, fails, warns, acks)
            # the tool's own advice is the one thing it cannot check — every
            # full report closes by inviting a report of its defects
            self._render_feedback_request()

        return self.exit_code(self.stale, fails, self.args)

    @staticmethod
    def exit_code(stale: list[str], fails: list[Action], args) -> int:
        """The gate verdict: stale baseline entries and fail actions both block;
        --warn renders everything informational and exits clean."""
        if args.warn:
            return 0
        if stale:
            # Phase 5: drift/fix/delete are the same case — the KEY matches no
            # current finding; the ledger (stdout) shows the count and remedy
            CONSOLE.warn(
                f"{len(stale)} stale baseline entr{'y' if len(stale) == 1 else 'ies'} match no current finding — "
                f"drifted, fixed, or deleted are the same case: {', '.join(stale[:5])}"
                f"{'...' if len(stale) > 5 else ''}; "
                f"run --update-baseline to re-lock the baseline"
            )
        if fails:
            CONSOLE.warn(f"{len(fails)} action(s) found — failing (use --warn to run informational)")
        return 1 if (stale or fails) else 0

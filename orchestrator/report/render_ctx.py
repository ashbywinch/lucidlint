"""The report: what the gate prints for a set of actions.

One context object holds the run's provenance (repo, branch, commit, coverage
label) and renders the three surfaces — the JSON document the machines read,
the gate summary, and the per-file text report. The pure line-builders are the
class's own static helpers, so a caller that has one action still reaches the
fact line and the seam groups without a context.
"""

from __future__ import annotations

import argparse
import datetime
import json
from collections import Counter
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

import rule_metadata
from orchestrator.action import Action
from orchestrator.baseline import Baseline
from orchestrator.report.report_document import ReportDocument
from orchestrator.report.report_meta import ReportMeta
from orchestrator.report.seam_group import SeamGroup
from orchestrator.version import VERSION


@dataclass
# lucidlint: ignore partition the render-context accessors are by-feature read helpers on one record
class RenderCtx:
    """The shared render context — one object instead of a 9-parameter tail."""

    # the config-ignored ledger lists this many families before counting the rest
    _LEDGER_TOP_FAMILIES = 4
    # a fact is worth a clause at these magnitudes (the scanner's own bars)
    _COMPLEX_FACTS_AT = 15
    _CHURN_FACTS_AT = 10
    _CALLERS_FACTS_AT = 10

    repo: Path
    args: argparse.Namespace
    branch: str
    commit: str
    coverage_source: str
    graph_preferred: bool
    ignored_by_signal: Counter | None = None
    report_header: str = ""
    suppression_census: dict[str, int] | None = None
    config_ignore_keys: dict[str, str] = field(default_factory=dict)  # B5: signal -> config key
    baseline_migration: str = ""  # B3: pre-round baseline note ("" = none)
    stale_count: int = 0  # Phase 5: acknowledged keys matching no current finding

    _NAMING_NOTICE = (
        "NAMING — a name is the commitment: what the domain calls the THING. Verb names "
        "(-er/-or) name a process — stateful process = name the state; stateless = the "
        "operations belong to the abstraction that owns their state — find it; an -er/-or "
        "is honest only when the domain calls a stateful component that. Generic containers "
        "(Options/Context/Parameters/Config) and tool jargon (Seam/Clump/Accumulator) name "
        "the means, not the thing."
    )

    # The report's closing ask (issue feedback): the gate checks this repo's
    # code, never the soundness of its own advice. A suggestion that would make
    # the reader's code worse, or a message the reader cannot act on, is a defect
    # only the reader can see — the rules and fixtures cannot supply a case the
    # rule does not yet exclude. Per-file runs stay silent (the LSP reprints the
    # report on the next edit) and --json stdout stays a machine document.
    FEEDBACK_REQUEST = (
        "feedback: if a lucidlint suggestion would make your code worse, or a message is confusing, open an\n"
        "  issue at github.com/ashbywinch/lucidlint/issues with the finding's text and why it misleads\n"
        "  agents: file that issue only with your user's permission"
    )

    # the display kind -> suppression family map: a variant's identity IS its
    # family, so the per-site marker advice never names the variant instead of
    # the family
    _FAMILY_OF_VARIANT = {
        variant: family for family, variants in rule_metadata.CATALOG.families().items() for variant in variants
    }

    def _config_ignored_detail(self) -> str:
        """The §9 debt ledger's per-kind detail: the config-ignored families
        and their counts (magic-number=2, ...) — without them the ignore can
        grow without the gate ever showing WHICH kinds it hides."""
        if not self.ignored_by_signal:
            return ""
        total = sum(self.ignored_by_signal.values())
        top_items = self.ignored_by_signal.most_common(RenderCtx._LEDGER_TOP_FAMILIES)
        top = ", ".join(f"{sig}={n}" for sig, n in top_items)
        # the listed families must reconcile with the total beside them — a
        # truncated list that sums short reads as a miscount (album round:
        # 238+201+123+37 = 599 against "627 config-ignored")
        rest = total - sum(n for _, n in top_items)
        if rest:
            top += f", +{rest} more"
        return f" ({top})"

    def _baseline_acknote(self) -> str:
        """H7: the baseline-activation clause rides the LEDGER (the single
        home of the numbers) — the gate repeats no count. Phase 4: a
        repo-root lucidlint.json holding acknowledged actions reads as "+0
        acknowledged" until --baseline is passed; the note attaches the flag
        itself so the reader learns it from the message (round-3 CONFUSING:
        "95 acknowledged action(s)" read as "+0 acknowledged")."""
        if self.args.baseline is not None or self.baseline_migration:
            return ""
        current, _ = Baseline.file_state(self.repo / "lucidlint.json")
        if not current:
            return ""
        return f" — {current} acknowledged in lucidlint.json — pass --baseline lucidlint.json to activate"

    def _suppression_ledger(self, fails: list[Action], warns: list[Action], acks: list[Action]) -> str | None:
        """H7: ONE additive line — the single home of the report's numbers:
        reported = fails + warnings and total = reported + acknowledged +
        config-ignored + comment-suppressed are stated HERE, and the gate
        line repeats none of them. The baseline-activation clause rides the
        acknowledged term; Phase 5 appends the stale clause to it too — "N
        acknowledged entries match no current finding" renders ONLY when
        nonzero. None only when there is literally nothing to state."""
        ignored = sum(self.ignored_by_signal.values()) if self.ignored_by_signal else 0
        census = sum(self.suppression_census.values()) if self.suppression_census else 0
        reported = len(fails) + len(warns)
        acknote = self._baseline_acknote()
        stale_clause = ""
        if self.stale_count:
            n = self.stale_count
            stale_clause = (
                f" — {n} acknowledged entr{'y' if n == 1 else 'ies'} "
                f"match{'es' if n == 1 else ''} no current finding — "
                "re-acknowledge with --update-baseline"
            )
        terms = (reported, acks, ignored, census, acknote, self.stale_count)
        if not any(terms):
            return None
        total = reported + len(acks) + ignored + census
        return (
            f"{total} findings — {reported} reported ({len(fails)} fails + {len(warns)} warnings) "
            f"+ {len(acks)} acknowledged (baseline){acknote}{stale_clause}"
            f" + {ignored} config-ignored{self._config_ignored_detail()}"
            f" + {census} comment-suppressed"
        )

    def render_json(self, unique: list[Action]) -> None:
        """The machine document: the actions verbatim, the run's provenance and
        suppression facts, and the seam groups."""
        meta = ReportMeta(
            repo=str(self.repo),
            tool_version=VERSION,
            branch=self.branch,
            commit=self.commit,
            generated_at=datetime.date.today().isoformat(),
            coverage_source=self.coverage_source,
            thresholds={
                "max_complexity": 15,
                "max_function_lines": 120,
                "max_dependent_modules": 15,
                "max_risk": 0.8,
                "hotspot_top_frac": 0.1,
                "hotspot_min_cc": 15.0,
            },
        )
        document = ReportDocument(
            meta=meta,
            baseline=str(self.args.baseline) if self.args.baseline else "",
            config_ignored=dict(self.ignored_by_signal) if self.ignored_by_signal else {},
            header=self.report_header,
            suppressions=dict(self.suppression_census or {}),
            actions=[asdict(a) for a in unique],
            naming_notice=(
                RenderCtx._NAMING_NOTICE if any(RenderCtx._stamp_of(a) == "JUDGEMENT" for a in unique) else None
            ),
            groups=RenderCtx._seam_groups(unique),
        )
        print(json.dumps(asdict(document), indent=2))

    def render_summary(self, fails: list[Action], warns: list[Action], acks: list[Action]) -> None:
        """Gate verdict, scope, and fail-bar lines."""
        args = self.args
        coverage_source = self.coverage_source
        graph_preferred = self.graph_preferred
        bits = []
        # H7: the counts live in the LEDGER — the gate repeats none of them;
        # it states the mode, the scope (distinct targets), and the fail
        # bars. The baseline honesty clause carries no count and stays here
        # ("no baseline — cannot tell what is new"); the activation clause
        # for a repo-root lucidlint.json rides the ledger's acknowledged
        # term (Phase 4: "95 acknowledged action(s)" read as "+0
        # acknowledged" — round-3 CONFUSING).
        # Phase 4: the highest change-cost line and the percentile legend
        # are GONE — the composite read as urgency (round-7: RISK99 beside
        # a rejected finding). The raw metrics stay, rendered per-item as
        # named facts (_facts_line) — never a percentile, never "risk".
        if args.baseline is None and not self.baseline_migration:
            current, _ = Baseline.file_state(self.repo / "lucidlint.json")
            if not current:
                bits.append("no baseline — cannot tell what is new")
        mine_txt = (", " + ", ".join(bits)) if bits else ""
        # a target is the (file, function) a fail anchors — say so; "distinct
        # targets" alone read as directories or languages
        targets = len({(a.file, a.function) for a in fails})
        verdict = "GATE: FAIL" if not args.warn else "GATE: INFORMATIONAL (--warn)"
        print(f"{verdict} — across {targets} failing target{'' if targets == 1 else 's'}{mine_txt}")
        if graph_preferred:
            print(
                "WARNING: coverage snapshot predates the repo's tests — hard 'untested' claims are suppressed; "
                "run --refresh-coverage (make coverage) for definite test-status verdicts"
            )
        print(
            "fail bars: CC>=15, fn>=120 lines, file>=150 edges, hotspot top 10% "
            f"by churn with CC>=15; coverage: {coverage_source}"
        )

    def render_text(self, unique: list[Action], fails: list[Action], warns: list[Action], acks: list[Action]) -> None:
        repo, args = self.repo, self.args
        if self.report_header:
            print(self.report_header)
            print()
            prov = f"lucidlint {VERSION}"
            if self.commit:
                prov += f" @ {self.commit}"
            print(prov)
            print()
        # #33(b): the naming lesson is issued ONCE per report, iff any finding
        # is judge-true — never copied onto every message
        if any(RenderCtx._stamp_of(a) == "JUDGEMENT" for a in unique):
            print(RenderCtx._NAMING_NOTICE)
            print()
        # #34: shared-seam clusters render ONE heading before the findings
        groups = RenderCtx._seam_groups(unique)
        if groups:
            print("\n".join(g["heading"] for g in groups))
            print()
        # B3: a pre-round baseline schema is a file-format problem, named ONCE
        if self.baseline_migration:
            print(self.baseline_migration)
            print()
        # H7: the LEDGER is the single home of the numbers — the additive
        # relationship (reported = fails + warnings; total = reported +
        # acknowledged + config-ignored + comment-suppressed) is stated ONCE
        # here, and every GATE line repeats none of it
        ledger = self._suppression_ledger(fails, warns, acks)
        if ledger:
            print(ledger)
            print()
        if not unique:
            # the ledger must show even when the config-ignores ate every
            # action — "clean" while debt is hidden is the invisibility the
            # ledger exists to remove (review finding)
            print("GATE: PASS — clean, no actions")
            return
        if not fails:
            print("GATE: PASS")
            if warns:
                print(f"by kind — warnings: {RenderCtx._kind_counts(warns)}")
                RenderCtx._render_actions(repo, args, warns, [], self.suppression_census, self.config_ignore_keys)
            return
        self.render_summary(fails, warns, acks)
        print(f"by kind — fails: {RenderCtx._kind_counts(fails)}; warnings: {RenderCtx._kind_counts(warns)}")
        RenderCtx._render_actions(repo, args, fails, acks, self.suppression_census, self.config_ignore_keys)
        if warns:
            print(f"\nwarnings (reported, never fail) — {len(warns)}:")
            # the census was printed with the fails group — once per report
            RenderCtx._render_actions(repo, args, warns, [], None, self.config_ignore_keys)

    @staticmethod
    def _facts_line(a: Action) -> str | None:
        """Phase 4: the raw metrics render as NAMED FACTS on one extra line —
        never a percentile, never "risk" (the composite read as urgency). A
        clause per notable fact: CC >= 15, or >= 10 changes in the history, or
        >= 10 callers; a metric that is absent (the scanner default of 1.0)
        skips its clause. The subject is "This function" — "This code" only
        when the finding anchors no function."""
        facts = []
        if a.metric >= RenderCtx._COMPLEX_FACTS_AT:
            facts.append(f"is complex ({int(a.metric)} decision points)")
        if a.churn >= RenderCtx._CHURN_FACTS_AT:
            facts.append(f"changed often ({a.churn} changes)")
        if len(a.callers) >= RenderCtx._CALLERS_FACTS_AT:
            facts.append(f"has {len(a.callers)} callers")
        if not facts:
            return None
        # the anchor field carries a function OR a class name (latent-class,
        # record-shape on a class) — "This function changed often" on a class is
        # a mislabel, so the line states the facts without a guessed subject
        if len(facts) == 1:
            sentence = facts[0]
        else:
            joiner = ", and " if len(facts) > 2 else " and "
            sentence = ", ".join(facts[:-1]) + joiner + facts[-1]
        return sentence[0].upper() + sentence[1:] + "."

    @staticmethod
    def _render_file_group(file: str, items: list[Action], ignore_keys: dict[str, str] | None = None) -> None:
        """One file's actions, priority-ordered, with notes."""
        print(f"\n{file}")
        for a in items:
            # the column anchors marker placement (schema-3): same-line twins
            # peel inner-first, and the report names WHICH twin
            loc = f":{a.line}" + (f":{a.col}" if a.col else "") + (f" ({a.function})" if a.function else "")
            churn = f" [churn {a.churn}x]" if a.churn else ""
            kinds = ",".join(a.kinds) if a.kinds else a.kind
            # Phase 4: the per-item RISKxx bracket is gone — it read as a
            # brokenness order (the valuable fail carried the lowest display
            # value); severity survives as the [warn] marker for warnings, and
            # the raw metrics render as the facts line (_facts_line) — never a
            # percentile, never "risk"
            tag = "warn" if a.severity == "warn" else ""
            # a latent-class variant's display kind (latent-class) IS its
            # suppression family — nothing to teach, and a suppression recipe
            # beside "create the class" invites hiding the defect. Only a
            # display identity that cannot be suppressed (standard) names the
            # raw signal.
            marker = (
                a.signal
                and a.signal != a.kind
                and a.signal not in a.kinds
                and a.signal not in RenderCtx._FAMILY_OF_VARIANT
            )
            # Phase 4: when a family's suppression identity and its fix are two
            # words (class-module -> split-module), the line links them — the
            # reader saw "suppress with: class-module" beside "fix: --kind
            # split-module" as two unrelated commands (round-3 CONFUSING)
            fix_link = f" (this family's fix: {a.fix_kind})" if a.fix_kind and a.fix_kind != a.signal else ""
            # B5: a bare per-site marker is the wrong remedy when the family is
            # config-ignored (a house decision, made elsewhere) — say so and name
            # the config key; otherwise state the marker window POSITIVELY: a
            # marker binds by its signal name on the finding's own line or within
            # the 3 lines ending at it — never advice to relocate a marker (Phase
            # 3: a mis-placed marker is documentation, not something to move).
            # G5: the why is free text — one cut mid-word at the line end still
            # binds, because the signal name is the binding token
            ck = (ignore_keys or {}).get(a.signal) if a.signal else None
            if marker and ck:
                suppress = (
                    f" — suppress with: {a.signal}{fix_link} — a config entry ignores this signal "
                    f"for {ck} only; this site is not covered — fix it, or add a per-site why here"
                )
            elif marker:
                suppress = (
                    f" — suppress with: {a.signal}{fix_link} — a lucidlint: ignore comment binds "
                    "by its signal name on this finding's own line, the line before it, the "
                    "logical line's first source line, or the line before that; the why text "
                    "may end mid-word at the line end and still bind"
                )
            elif ck:
                suppress = f" — this signal is config-ignored under {ck}"
            else:
                suppress = ""
            stamp = RenderCtx._stamp_of(a)
            bracket = "".join(f"[{t}]" for t in (tag, kinds) if t)
            # H6: the MESSAGE leads — the suppression pointer (and the
            # config-ignored advisory / marker-window sentence where they apply)
            # appends AFTER it, so the action reads first and the pointer
            # appears exactly once (the messages no longer embed suppression
            # wording — the pointer is renderer data)
            print(f"  {bracket} {loc}{churn} — {a.message}{suppress}" + (f" [{stamp}]" if stamp else ""))
            # Phase 4: the raw metrics render right under the message as NAMED
            # FACTS (one extra line, a clause per notable fact) — the composite
            # and its percentile left the report (see _facts_line)
            facts = RenderCtx._facts_line(a)
            if facts:
                print(f"      -> {facts}")
            if a.note:
                print(f"      -> {a.note}")

    @staticmethod
    def _fix_of(a: Action) -> rule_metadata.Fix | None:
        """The per-finding Fix (Fifth-pass): constructed from the finding's
        STRUCTURED fix_kind (schema 4) + its anchor — the stamps, the NAMING
        notice trigger, and command() all read this object, never message text."""
        if not a.fix_kind:
            return None  # fix-less — never constructed, never stamped
        return rule_metadata.Fix(kind=a.fix_kind, file=a.file, line=a.line, col=a.col)

    @staticmethod
    def _stamp_of(a: Action) -> str:
        """#33(a)+H5: render-time only — `JUDGEMENT` when the finding's fix needs
        a name (the registry says so via name_required) OR its kind is
        judgement-marked (H5: split-module's split is a grouping decision — a
        judgement, not a mechanical fact, even though its package name is
        optional); `MECHANICAL` for fixable kinds outside both; fix-less
        findings get neither."""
        fix = RenderCtx._fix_of(a)
        if fix is None:
            return ""
        return "JUDGEMENT" if fix.judgement else "MECHANICAL"

    # lucidlint: ignore closures the nested visitors are one-purpose probes — hoisting names nothing the domain owns
    @staticmethod
    def _seam_groups(actions: list[Action]) -> list[SeamGroup]:
        """#34: N findings sharing ONE seam are one design decision. Seams:
        (a) complexity/large-function -> (file, function); (b) the carriers
        (data-clump/partition/strewing) -> their seam_members sets. Union-find
        transitive closure over overlapping seams (cross-kind groups allowed).
        Groups with >=3 findings render a heading; the array is ALWAYS present
        (empty when no cluster)."""

        def seam_of(a: Action) -> Any:
            if a.kind in ("complexity", "large-function"):
                return ("loc", a.file, a.function)
            if a.seam_members:
                return ("members", frozenset(a.seam_members))
            return None

        seams = [seam_of(a) for a in actions]
        parent = list(range(len(actions)))

        def find(i: int) -> int:
            while parent[i] != i:
                parent[i] = parent[parent[i]]
                i = parent[i]
            return i

        def union(i: int, j: int) -> None:
            ri, rj = find(i), find(j)
            if ri != rj:
                parent[ri] = rj

        for i in range(len(actions)):
            si = seams[i]
            if si is None:
                continue
            for j in range(i + 1, len(actions)):
                sj = seams[j]
                if sj is None or si[0] != sj[0]:
                    continue
                if si[0] == "loc":
                    if si == sj:
                        union(i, j)
                elif not si[1].isdisjoint(sj[1]):
                    union(i, j)

        by_root: dict[int, list[int]] = {}
        for i in range(len(actions)):
            by_root.setdefault(find(i), []).append(i)
        groups = []
        for members in by_root.values():
            if len(members) < 3:
                continue
            members_sorted = sorted(members, key=lambda i: (actions[i].file, actions[i].line, i))
            member_seams = [seams[i] for i in members_sorted if seams[i] is not None]
            shared_loc = next((s for s in member_seams if s[0] == "loc"), None)
            shared_members: set[str] | None = None
            for s in member_seams:
                if s[0] == "members":
                    shared_members = set(s[1]) if shared_members is None else shared_members & set(s[1])
            seam_label = (
                f"{shared_loc[1]} ({shared_loc[2]})"
                if shared_loc is not None
                else (", ".join(sorted(shared_members)) if shared_members else "")
            )
            # B1: an empty member list must never render "(the seams: )" — a
            # transitive cluster has no member present in every finding, so there
            # is no seam NAME to print; drop the parenthetical instead
            paren = f" (the seams: {seam_label})" if seam_label else ""
            groups.append(
                SeamGroup(
                    heading=(
                        f"these {len(members_sorted)} findings share ONE seam — design the target "
                        f"type once, for all of them{paren}"
                    ),
                    seam=seam_label,
                    findings=members_sorted,
                )
            )
        groups.sort(key=lambda g: (actions[g["findings"][0]].file, actions[g["findings"][0]].line))
        return groups

    @staticmethod
    def _kind_counts(actions: list[Action]) -> str:
        """Category-count roll-up: record-shape=261, standard=199, ... — the
        volume each rule contributes, so a 500-action report is scannable."""
        counts = Counter(a.kind for a in actions)
        return ", ".join(f"{k}={v}" for k, v in counts.most_common())

    @staticmethod
    # lucidlint: ignore closures,long-param-list the render helpers are one-purpose local walks; the context is a trio
    def _render_actions(
        repo: Path,
        args,
        fails: list[Action],
        acks: list[Action],
        census: dict[str, int] | None = None,
        ignore_keys: dict[str, str] | None = None,
    ) -> None:
        """Per-file grouped action lines, baseline acknowledgements, the footer,
        and the suppression census (what the gate did NOT report on)."""
        # B6: a bulk-suppression finding is a repo-wide count — its anchor file
        # is arbitrary (the file with the most suppressed sites), so it renders
        # ONCE at report level, never inside a per-file section
        bulk = [a for a in fails if a.kind == "bulk-suppression"]
        rest = [a for a in fails if a.kind != "bulk-suppression"]
        if bulk:
            print("\nbulk suppression (policy — a repo-level decision, not a file site):")
            for a in sorted(bulk, key=lambda a: a.message):
                print(f"  {a.message}")
        by_file: dict[str, list[Action]] = {}
        for a in rest:
            by_file.setdefault(a.file, []).append(a)
        for file, items in sorted(by_file.items(), key=lambda kv: -max(i.priority for i in kv[1])):
            RenderCtx._render_file_group(file, items, ignore_keys)
        if acks:
            print(
                f"\nacknowledged in baseline ({len(acks)}): "
                + ", ".join(f"{a.file}:{a.line}" for a in acks[:5])
                + (" …" if len(acks) > 5 else "")
            )
        if census:
            ranked = sorted(census.items(), key=lambda kv: -kv[1])
            print("suppressed: " + " ".join(f"{sig}×{n}" for sig, n in ranked))
        print(
            "\nre-run: python3 lucidlint.py --repo "
            + str(repo)
            + (" --baseline " + str(args.baseline) if args.baseline else "")
            + "   | tool lives in lucidlint (github.com/ashbywinch/lucidlint); thresholds and"
            + " per-action data in --json output"
        )
        print(
            "baseline: '--update-baseline --baseline lucidlint.json' acknowledges today's debt so the "
            "gate only fails on NEW actions; this report is a snapshot, not wired into CI"
        )

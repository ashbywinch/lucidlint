"""The auto-fix engine — libcst transforms for a scanner finding.

A finding carries (kind, file, line); each kind has a lossless transform
(libcst round-trips comments and formatting). The agent workflow:

    lucidlint.py fix --kind stale-suppression --file x.py --line 12

The transform edits the working tree; the gate re-run confirms the finding
is gone. libcst is optional (extra `fix`) — without it the engine reports
the missing dependency and exits non-zero; nothing is half-applied.

The package is the public surface only: one module per concern — the request
and its kind tables (`request`), the shared mechanics (`bindings`,
`module_outline`, `imports`, `rewrite`, `module_source`, `source_files`), the
seam analysis (`method_extraction`) — and one fixer class per module under
`fixers/`. Everything a consumer or a test reaches for is re-exported here.
"""

from __future__ import annotations

from libcst.metadata import PositionProvider as PositionProvider

from fix_engine import request as _request
from fix_engine.bindings import Bindings
from fix_engine.fix_options import FixOptions as FixOptions
from fix_engine.fixers.module_splitting import ModuleSplitting
from fix_engine.method_extraction import MethodExtraction
from fix_engine.module_outline import ModuleOutline
from fix_engine.rewrite import Rewrite
from fix_engine.source_files import SourceFiles

# the request and the kind tables the fix command reads
_FixRequest = _request._FixRequest
MECHANICAL_KINDS = _request.MECHANICAL_KINDS
STRUCTURAL_KINDS = _request.STRUCTURAL_KINDS
PREVIEW_KINDS = _request.PREVIEW_KINDS
KIND_ALIASES = _request.KIND_ALIASES
_NAME_REQUIRED_KINDS = _request._NAME_REQUIRED_KINDS
_STRUCTURAL_FIXERS = _request._STRUCTURAL_FIXERS
# Every kind the fix command can actually apply. A finding message may
# restrict advertising to what the fix command can actually apply: a
# finding message may advertise "— fix: <kind>" ONLY when the kind is
# here — suggesting a fix that cannot apply sends agents hand-editing
# around working automation, or guessing at a seam. (Rust-file kinds are
# guarded by the Rust binary's own refusal; Python kinds refused by the
# gate's unknown-kind guard.)
FIXABLE_KINDS = set(MECHANICAL_KINDS) | set(_STRUCTURAL_FIXERS) | {"extract-module"}

# lucidlint: ignore-file global-state the package root re-exports its modules' names —
# lucidlint: ignore-file global-state an alias is the import surface, not state
# the shared mechanics stay reachable from the package root: consumers and the
# tests import them from fix_engine
_as_range = ModuleOutline.span
_name_ref_kind = ModuleSplitting._name_ref_kind
_exec_time_names = ModuleSplitting._exec_time_names
_as_node = ModuleOutline.resolve_node
_iter_nodes = ModuleOutline.iter_nodes
_iter_nodes_pruned = ModuleOutline.iter_nodes_pruned
_class_body_binds = Bindings.class_body_binds
_expr_names = Bindings.expr_names
_collect_alias_names = Bindings.collect_alias_names
_is_keyword_name = ModuleOutline.is_keyword_name
_is_self_attribute = ModuleOutline.is_self_attribute
_import_base_name = Bindings.import_base_name
_is_docstring_stmt = ModuleOutline.is_docstring_stmt

_py_files = SourceFiles.py_files
_record_class_def = Rewrite.record_class_def
_self_attr_target = Rewrite.self_attr_target
_hoist_target_names = Bindings.hoist_target_names
_extraction_is_private = Rewrite.extraction_is_private
_RemoveNodes = Rewrite._RemoveNodes

# the method-extraction mechanics stay reachable from the package root (the tests
# and the scanner-side helpers import them there)
_Analyse = MethodExtraction._Analyse
_DecisionCount = MethodExtraction._DecisionCount
_ExtractMethodRewrite = MethodExtraction._ExtractMethodRewrite
_FnBodyState = MethodExtraction._FnBodyState
_InsertExtractedFn = MethodExtraction._InsertExtractedFn
_chain_operands = MethodExtraction._chain_operands
_for_target_names = MethodExtraction._for_target_names
_is_nested_target = MethodExtraction._is_nested_target
_is_rebound_read = MethodExtraction._is_rebound_read
_min_seam_decisions = MethodExtraction._min_seam_decisions
_module_level_names = MethodExtraction._module_level_names
_stmt_decision_count = MethodExtraction._stmt_decision_count
_target_bound_names = MethodExtraction._target_bound_names
_wildcard = MethodExtraction._wildcard
collect_stmt_names = MethodExtraction.collect_stmt_names

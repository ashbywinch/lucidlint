"""Tests for check_records.py (moved from omp-config tools) (stdlib unittest — no deps in this repo).

Fixtures are REAL files under tools/tests/fixtures/ — the coding standard
forbids source code embedded in test strings (invisible to the type
checker, drifts from the real toolchain, cannot be linted or run).

Run: python3 -m unittest discover -s tools/tests
Wired into `make test` alongside the other self-checks.
"""

import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from check_records import ScanResult, scan  # noqa: E402

FIXTURES = Path(__file__).resolve().parent / "fixtures"


def scan_fixture(name: str) -> ScanResult:
    """Run the gate on one fixture file."""
    return scan([FIXTURES / name])


class RecordCollectionGateTest(unittest.TestCase):
    def test_dict_str_signature_is_a_map_exempt(self):
        self.assertEqual(scan_fixture("dict_str_signature.py").findings, [])

    def test_primitive_map_is_exempt(self):
        self.assertEqual(scan_fixture("map_counts.py").findings, [])

    def test_dict_any_return_is_a_finding(self):
        self.assertEqual(len(scan_fixture("dict_any_return.py").findings), 1)

    def test_tuple_signature_is_a_finding(self):
        findings = scan_fixture("tuple_signature.py").findings
        self.assertEqual(len(findings), 1)
        self.assertIn("tuple[int, bool]", findings[0])

    def test_nested_list_is_a_finding(self):
        self.assertEqual(len(scan_fixture("nested_list_signature.py").findings), 1)

    def test_list_of_primitive_dict_is_a_finding(self):
        self.assertEqual(len(scan_fixture("list_of_primitive_dict.py").findings), 1)

    def test_dict_of_collection_is_a_finding(self):
        self.assertEqual(len(scan_fixture("dict_of_collection.py").findings), 1)

    def test_map_of_domain_class_is_exempt(self):
        self.assertEqual(scan_fixture("map_of_domain_ok.py").findings, [])

    def test_list_of_domain_class_is_exempt(self):
        self.assertEqual(scan_fixture("list_of_domain_ok.py").findings, [])

    def test_list_of_optional_domain_is_exempt(self):
        """list[Optional[Label]] is a collection of a domain class, exactly
        like list[Label] — the Optional wrapper must be peeled first."""
        self.assertEqual(scan_fixture("list_of_optional_domain_ok.py").findings, [])

    def test_list_union_dict_value_is_a_finding(self):
        """list[dict[str, Any] | None] is a collection of grab-bags, exactly
        like list[Optional[dict[str, Any]]] — equivalent spellings agree."""
        self.assertEqual(len(scan_fixture("list_union_dict_value.py").findings), 1)

    def test_list_capitalized_typing_element_is_a_finding(self):
        """List[Dict] (legacy capitalized typing spellings) is a collection
        of grab-bags, exactly like list[dict] — case must not flip the verdict."""
        self.assertEqual(len(scan_fixture("list_capitalized_typing.py").findings), 1)

    def test_list_of_primitive_is_exempt(self):
        self.assertEqual(scan_fixture("list_of_str_ok.py").findings, [])

    def test_deserializer_boundary_is_exempt(self):
        self.assertEqual(scan_fixture("deserializer_boundary.py").findings, [])

    def test_wrapped_deserializer_return_is_exempt(self):
        self.assertEqual(scan_fixture("wrapped_deserializer.py").findings, [])

    def test_optional_deserializer_return_is_exempt(self):
        self.assertEqual(scan_fixture("optional_deserializer.py").findings, [])

    def test_union_value_map_is_exempt(self):
        self.assertEqual(scan_fixture("union_value_map.py").findings, [])

    def test_grab_bag_to_primitive_is_a_finding(self):
        self.assertEqual(len(scan_fixture("grab_bag_to_primitive.py").findings), 1)

    def test_record_dict_literal_is_a_finding(self):
        findings = scan_fixture("record_literal.py").findings
        self.assertEqual(len(findings), 1)
        self.assertIn("record", findings[0])

    def test_lookup_table_is_exempt(self):
        self.assertEqual(scan_fixture("lookup_table_ok.py").findings, [])

    def test_mixed_record_literal_is_a_finding(self):
        self.assertEqual(len(scan_fixture("mixed_record_literal.py").findings), 1)

    def test_headers_map_literal_is_exempt(self):
        self.assertEqual(scan_fixture("headers_map.py").findings, [])

    def test_assigned_record_literal_is_a_finding(self):
        self.assertEqual(len(scan_fixture("assigned_record.py").findings), 1)

    def test_inline_arg_record_literal_is_exempt(self):
        self.assertEqual(scan_fixture("inline_arg_record.py").findings, [])

    def test_optional_wrapped_return_is_a_finding(self):
        self.assertEqual(len(scan_fixture("optional_return_record.py").findings), 1)

    def test_union_param_record_is_a_finding(self):
        self.assertEqual(len(scan_fixture("union_param_record.py").findings), 1)

    def test_variadic_tuple_is_exempt(self):
        self.assertEqual(scan_fixture("variadic_tuple_ok.py").findings, [])

    def test_nested_constant_lookup_is_exempt(self):
        self.assertEqual(scan_fixture("nested_const_lookup_ok.py").findings, [])

    def test_comprehension_record_is_a_finding(self):
        self.assertEqual(len(scan_fixture("comprehension_record.py").findings), 1)

    def test_typing_qualified_record_is_a_finding(self):
        self.assertEqual(len(scan_fixture("typing_qualified.py").findings), 1)

    def test_bulk_deserializer_is_exempt(self):
        """from_lines(list[dict[str, Any]]) -> list[Label] is raw JSON in,
        domain objects out — the sanctioned boundary."""
        self.assertEqual(scan_fixture("bulk_deserializer.py").findings, [])

    def test_nested_dict_lookup_is_exempt(self):
        self.assertEqual(scan_fixture("nested_dict_lookup_ok.py").findings, [])

    def test_typing_qualified_wrapper_is_a_finding(self):
        """typing.Optional[dict[str, Any]] unwraps like Optional[dict[str, Any]]."""
        self.assertEqual(len(scan_fixture("typing_qualified_wrapper.py").findings), 1)

    def test_optional_value_map_is_exempt(self):
        """dict[str, Optional[str]] is a map, exactly like dict[str, str | None]."""
        self.assertEqual(scan_fixture("optional_value_map_ok.py").findings, [])

    def test_vararg_and_kwarg_annotations_are_findings(self):
        self.assertEqual(len(scan_fixture("varargs.py").findings), 2)

    def test_union_grab_bag_value_is_a_finding(self):
        """dict[str, Any | None] is as shapeless as dict[str, Any]."""
        self.assertEqual(len(scan_fixture("union_grab_bag.py").findings), 1)

    def test_union_grab_bag_at_boundary_is_exempt(self):
        self.assertEqual(scan_fixture("union_grab_bag_boundary.py").findings, [])

    def test_mixed_key_record_is_a_finding(self):
        """The tighter literal rule: a constant string key and a dynamic
        value make a record, whatever the odd key is."""
        self.assertEqual(len(scan_fixture("mixed_key_record.py").findings), 1)

    def test_map_return_is_not_a_deserializer_boundary(self):
        """dict[str, Label] return is a map, not a domain-class return — the
        grab-bag parameter is not silently exempted."""
        findings = scan_fixture("map_return_not_boundary.py").findings
        self.assertEqual(len(findings), 1)
        self.assertIn("parameter 'd'", findings[0])


class StrewingWarningTest(unittest.TestCase):
    def test_three_shared_params_warns_not_fails(self):
        result = scan_fixture("three_shared.py")
        self.assertEqual(result.findings, [])  # strewing is a warning, not the gate
        self.assertEqual(len(result.warnings), 1)
        self.assertIn("3 free functions", result.warnings[0])

    def test_two_shared_params_warns(self):
        result = scan_fixture("two_shared.py")
        self.assertEqual(result.findings, [])
        self.assertEqual(len(result.warnings), 1)
        self.assertIn("2 free functions", result.warnings[0])

    def test_different_leading_params_is_clean(self):
        result = scan_fixture("different_leads.py")
        self.assertEqual(result.findings, [])
        self.assertEqual(result.warnings, [])

    def test_class_methods_are_ignored(self):
        result = scan_fixture("methods_only.py")
        self.assertEqual(result.findings, [])
        self.assertEqual(result.warnings, [])

    def test_unannotated_first_params_are_skipped(self):
        result = scan_fixture("unannotated.py")
        self.assertEqual(result.findings, [])
        self.assertEqual(result.warnings, [])

    def test_directory_walk_and_single_file(self):
        with tempfile.TemporaryDirectory() as td:
            shutil.copy2(FIXTURES / "three_shared.py", Path(td) / "a.py")
            shutil.copy2(FIXTURES / "different_leads.py", Path(td) / "b.py")
            self.assertEqual(len(scan([Path(td)]).warnings), 1)
            self.assertEqual(len(scan([Path(td) / "a.py"]).warnings), 1)
            self.assertEqual(scan([Path(td) / "b.py"]).warnings, [])

    def test_broken_source_is_skipped_not_crashed(self):
        result = scan_fixture("broken.py")
        self.assertEqual(result.findings, [])
        self.assertEqual(result.warnings, [])


class FixtureDirSkipTest(unittest.TestCase):
    def test_fixture_directories_are_skipped(self):
        """Fixture files are intentionally non-compliant test input; the
        gate must not flag its own test corpus."""
        result = scan([FIXTURES])
        self.assertEqual(result.findings, [])
        self.assertEqual(result.warnings, [])


if __name__ == "__main__":
    unittest.main()

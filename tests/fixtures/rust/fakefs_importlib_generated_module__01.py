import importlib.util


def test_generated_module_is_imported(tmp_path):
    path = tmp_path / "generated.py"
    path.write_text("VALUE = 42\n")
    spec = importlib.util.spec_from_file_location("generated", path)
    assert spec is not None

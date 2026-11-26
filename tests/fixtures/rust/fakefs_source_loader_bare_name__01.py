from importlib.machinery import SourceFileLoader


def test_loads_a_generated_module(tmp_path):
    path = tmp_path / "generated.py"
    path.write_text("VALUE = 1\n")
    loader = SourceFileLoader("generated", str(path))
    assert loader is not None

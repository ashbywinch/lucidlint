import importlib.util
from importlib.machinery import SourceFileLoader


def test_loads_a_generated_module(tmp_path):
    path = tmp_path / "generated.py"
    path.write_text("VALUE = 1\n")
    loader = SourceFileLoader("generated", str(path))
    spec = importlib.util.spec_from_loader("generated", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    assert module.VALUE == 1

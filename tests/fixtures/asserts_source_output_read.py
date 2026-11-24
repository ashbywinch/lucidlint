from pathlib import Path


def test_output_read_assertion(tmp_path):
    path = tmp_path / "app.py"
    path.write_text("def f_append(items):\n    return items\n")
    out = path.read_text()
    assert "def f_append(" in out

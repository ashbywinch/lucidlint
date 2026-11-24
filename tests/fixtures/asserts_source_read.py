from pathlib import Path


def test_source_read_assertion():
    src = Path("src/app.py").read_text()
    assert "def f_append(" in src

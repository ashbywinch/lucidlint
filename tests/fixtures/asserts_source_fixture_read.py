from pathlib import Path


def test_fixture_read_assertion():
    src = Path("tests/fixtures/loop_pipeline.py").read_text()
    assert "def f_append(" in src

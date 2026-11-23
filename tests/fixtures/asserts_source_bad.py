from pathlib import Path


def test_needle_is_code():
    src = load_source()
    assert "def f(" in src


def test_output_text_is_code():
    src = load_source()
    out = fix(src)
    assert "class Foo" in out


def test_code_text_variable_prose_needle():
    text = Path("src/app.py").read_text()
    assert "filter_map" in text


def test_code_literal_comparison():
    src = load_source()
    assert src == "def f():\n    pass\n"

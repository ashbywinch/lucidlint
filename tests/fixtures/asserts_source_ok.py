from tests.fixtures.helpers import fixture


def test_fixer_output_keeps_the_loop_shape():
    src = fixture("loop_pipeline.py")
    out, fixed = apply_loop_pipeline(src)
    assert "for " in fixed
    assert out


def test_parser_output_is_the_user_outcome():
    src = fixture("loop_pipeline.py")
    formatted = format_module(src)
    assert "def collect(" in formatted

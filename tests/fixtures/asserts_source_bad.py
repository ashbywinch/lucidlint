from tests.fixtures.helpers import fixture


def test_loop_shape_is_in_the_corpus():
    src = fixture("loop_sequence_sibling_branches.rs")
    assert "fn same_block(" in src


def test_bare_assertion_helper():
    src = fixture("loop_sequence_sibling_branches.rs")
    assertEqual(src, "fn same_block(")


def test_conditional_fixture_load(debug):
    src = fixture("loop_sequence_sibling_branches.rs")
    if debug:
        src = fixture("loop_sequence_sibling_branches.rs")
    assert "fn same_block(" in src

from tests.fixtures.helpers import fixture


def test_loop_shape_is_in_the_corpus():
    src = fixture("loop_sequence_sibling_branches.rs")
    assert "fn same_block(" in src

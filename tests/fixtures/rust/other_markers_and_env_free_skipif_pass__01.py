import pytest

@pytest.mark.parametrize('x', [1, 2])
def test_x(x):
    assert x

@pytest.mark.skipif(True, reason='tmp')
def test_y():
    pass

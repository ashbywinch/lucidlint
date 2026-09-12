import pytest
import os

@pytest.mark.skipif('os.environ' in os.environ, reason='env')
def test_x():
    pass

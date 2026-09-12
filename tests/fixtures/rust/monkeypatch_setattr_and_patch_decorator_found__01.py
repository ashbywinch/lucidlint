from unittest import mock

def test_x(monkeypatch):
    monkeypatch.setattr(Obj, 'x', 1)

@mock.patch('y')
def test_y():
    pass

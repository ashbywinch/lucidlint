def test_shape():
    src = fixture("x.rs")
    assert "def f(" in src

def test_shape():
    src = fixture("x.rs")
    assert "fn f(" in src

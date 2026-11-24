def test_findings_are_parsed():
    src = load_source()
    findings = scan(src)
    assert findings
    assert findings[0].kind == "magic-number"


def test_prose_needle_in_log():
    assert "error: bad" in stderr


def test_finding_message_is_output():
    finding = scan(load_source())[0]
    assert "fn f(" in finding.message

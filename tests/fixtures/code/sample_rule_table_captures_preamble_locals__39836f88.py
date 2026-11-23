def check(assessment, who):
    facts = assessment.get("facts", [])
    violations = []
    if facts and who.strip():
        violations.append("facts without a narrator")
    if not facts:
        violations.append("no facts at all")
    if who.strip() and len(facts) > 3:
        violations.append("too many facts")
    return violations

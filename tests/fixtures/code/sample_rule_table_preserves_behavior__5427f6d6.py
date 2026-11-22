def check(assessment, who):
    violations = []
    if who.strip() and assessment.get("q"):
        violations.append("the narrator is not known")
    if assessment.get("facts"):
        violations.append("facts without a date")
    if assessment.get("ages") is None:
        violations.append("no ages recorded")
    return violations

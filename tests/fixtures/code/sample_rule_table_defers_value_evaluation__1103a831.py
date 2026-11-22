def check(d):
    violations = []
    if d.get("k"):
        violations.append(d["k"])
    if d.get("m"):
        violations.append(d["m"] + 1)
    if "n" in d:
        violations.append(d["n"])
    return violations

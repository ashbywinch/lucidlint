def validate(rows):
    issues = []
    for s in rows:
        try:
            parse(s)
        except ValueError as e:
            issues.append(str(e))
    return issues

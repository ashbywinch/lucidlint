def f(rows):
    issues = []
    try:
        parse(rows)
    except ValueError as e:
        issues.append(str(e))
    return 'done'

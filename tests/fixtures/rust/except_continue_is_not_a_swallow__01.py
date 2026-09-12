def f(rows):
    for r in rows:
        try:
            parse(r)
        except ValueError:
            continue
    return 1

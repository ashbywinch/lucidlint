def g(paths):
    best = None
    for p in paths:
        m = p.name
        v = parse(m)
        if best is None or v > best:
            best = v
    return best
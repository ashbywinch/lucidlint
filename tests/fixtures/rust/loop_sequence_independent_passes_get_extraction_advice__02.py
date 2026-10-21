def latest(paths):
    best = None
    for p in paths:
        m = p.name
        version = 1
        if best is None or version > best:
            best = version
    return best
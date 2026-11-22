def chained_target(items):
    out = {}
    for item in items:
        out.setdefault(item, []).append(item)
    return out

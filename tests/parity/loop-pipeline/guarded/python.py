def guarded(items):
    out = []
    for item in items:
        if item > 0:
            out.append(item)
    return out

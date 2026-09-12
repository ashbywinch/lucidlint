def h(xs):
    out = []
    counts = {}
    total = 0
    for x in xs:
        out.append(x)
        counts[x] = counts.get(x, 0) + 1
        total += x
    return out, counts, total
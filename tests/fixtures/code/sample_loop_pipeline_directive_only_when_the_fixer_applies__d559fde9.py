def chained(rules):
    groups = {}
    for r in rules:
        groups.setdefault(r.kind, set()).add(r.severity)
    return groups


def moved(rules):
    out = []
    seed(out)
    for r in rules:
        out.append(r)
    return out


def built(rules):
    out = []
    for r in rules:
        out.append(r)
    return out

em = {r["id"]: (p, n) for r in []}
def f(em):
    return [x for x in em if em[x][0]]
def g(em):
    return {k: em[k][1] for k in em}
def h(em):
    return sorted(em, key=lambda k: em[k][1])

em = {r["id"]: (p, n) for r in epics}
def render(em):
    for cid, (p, nm) in em.items():
        if em[cid][0]:
            print(nm)
    a, b = em[cid]
    return a, b

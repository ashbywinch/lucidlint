def f(recs):
    names = []
    for r in recs:
        names.append(r["name"])
    shown = []
    for n in names:
        shown.append(n.upper())
    return shown
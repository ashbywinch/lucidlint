# Intentionally-shaped input for the loop-hoist fixer — single-accumulator
# loops whose bodies compute more than they append (the shapes the fixer
# rewrites into a per-item helper + flattening comprehension).


def h_filtered(items):
    out = []
    for it in items:
        x = it * 2
        if x > 0:
            out.append(x)
    return out


def h_multi(people):
    offenders = []
    for person in people:
        text = person.get("bio", "")
        if person.get("clarification"):
            offenders.append(person["name"])
        if person.get("reflection"):
            offenders.append(person["id"])
    return offenders


def h_set(items):
    seen = set()
    for it in items:
        x = it * 2
        seen.add(x)
    return seen
# Intentionally-shaped input for the pipeline fixer — shapes it must DECLINE
# with a reason, never half-apply.


def f_update(items):
    out = {}
    for x in items:
        out.update({x: 1})
    return out


def f_nonempty_init(items):
    out = [1]
    for x in items:
        out.append(x)
    return out
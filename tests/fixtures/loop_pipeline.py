# Intentionally-shaped input for the pipeline fixer — one comprehension shape
# per function. The declined shapes live in loop_pipeline_bad.py.


def f_append(items):
    out = []
    for x in items:
        out.append(x)
    return out


def f_filtered(items):
    out = []
    for x in items:
        if x > 0:
            out.append(x)
    return out


def f_extend(items):
    out = []
    for x in items:
        out.extend(g(x))
    return out


def f_set(items):
    seen = set()
    for x in items:
        seen.add(x)
    return seen
# Intentionally-shaped input for the loop-hoist fixer — bodies the rewrite
# MUST refuse (mid-build reads, extra accumulators, other outer writes,
# non-empty init, non-append accumulation).


def b_read(items):
    out = []
    for it in items:
        if it in out:
            out.append(it)
    return out


def b_two(items):
    out = []
    got = []
    for it in items:
        got.append(it)
        out.append(it * 2)
    return out, got


def b_counts(items):
    out = []
    counts = {}
    for it in items:
        counts[it] = 1
        out.append(it)
    return out, counts


def b_init(items):
    out = [1]
    for it in items:
        x = it * 2
        out.append(x)
    return out


def b_extend(items):
    out = []
    for it in items:
        out.extend(it)
    return out


def b_nested(items):
    out = []
    for it in items:
        x = it * 2

        def _fmt(v):
            got = v + 1
            return got

        out.append(_fmt(x))
    return out
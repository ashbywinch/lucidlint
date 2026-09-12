# A chain the loop-sequence fixer must REFUSE — a non-loop statement between
# the two accumulators breaks contiguity.


def interleaved(xs):
    out = []
    for x in xs:
        out.append(x)
    print(out)
    for x in xs:
        out.append(x)
    return out
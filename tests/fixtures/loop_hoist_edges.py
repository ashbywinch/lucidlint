def h_else(items):
    out = []
    for x in items:
        out.append(g(x))
    else:
        pass
    return out


def h_pair(pairs):
    out = []
    for key, value in pairs:
        out.append(value)
    return out


async def h_async(items):
    out = []
    async for x in items:
        out.append(g(x))
    return out


def h_no_init(items):
    for x in items:
        out.append(g(x))
    return out


def h_prior(items):
    out = [1]
    for x in items:
        out.append(g(x))
    return out

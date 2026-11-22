MODULE_GROUPS = {}
for module_rule in RULES:
    MODULE_GROUPS.setdefault(module_rule.kind, set()).add(module_rule.severity)


def group(xs):
    out = {}
    for x in xs:
        out.setdefault(x, []).append(x)
    return out


def chained_attribute(rows):
    out = Bag()
    for row in rows:
        out.items.append(row)
    return out


def loop_else(xs):
    out = []
    for x in xs:
        out.append(x)
    else:
        out.append(0)
    return out


def self_feed():
    xs = []
    for x in xs:
        xs.append(x)
    return xs


def plain_build(xs):
    out = []
    for x in xs:
        out.append(x)
    return out

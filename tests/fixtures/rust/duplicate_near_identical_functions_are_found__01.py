def price_a(items):
    total = 0
    for it in items:
        total += it.cost
    return total

def price_b(parts):
    total = 0
    for p in parts:
        total += p.cost
    return total

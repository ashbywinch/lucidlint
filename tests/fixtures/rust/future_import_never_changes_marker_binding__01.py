def stats(vals) -> tuple[int, int]:
    lo = min(vals)
    hi = max(vals)
    return lo, hi

xs = stats([3, 1, 2])
print(xs[0], xs[1])

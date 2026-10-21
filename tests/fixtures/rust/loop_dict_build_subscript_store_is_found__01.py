def build(items):
    result = {}
    for k, v in items:
        result[k] = f(v)
    return result
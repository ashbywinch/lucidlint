def process(data, factor):
    results = []
    for item in data:
        results.append(item * factor)
    return sum(results)

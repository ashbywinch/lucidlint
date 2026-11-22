def traversal(seeds):
    work = list(seeds)
    seen = []
    while work:
        node = work.pop()
        seen.append(node)
        work.extend([node])
    return seen

class Wall:
    pass


WallAlias = Wall


def combine(first: WallAlias, second: WallAlias):
    return first, second


def union(first: Wall, second: Wall):
    return first, second


def intersect(first: Wall, second: Wall):
    return first, second

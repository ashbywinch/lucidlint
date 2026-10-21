# Intentionally-shaped input for the loop-family gate test — the three shapes
# the report must carry in one scan: a loop-sequence pipeline, a loop-hoist
# single-mutation body, and a mutating-loop pair.


def all_text(people):
    chunks: list[str] = []
    for person in people:
        chunks += [person["name"]]
    for person in people:
        chunks += [person["bio"]]
    return chunks


def process(items):
    out = []
    for it in items:
        raw = it * 2
        trimmed = raw - 1
        if trimmed > 0:
            out.append(trimmed)
    return out


def tally(rows):
    out, counts = [], {}
    for r in rows:
        out.append(r)
        counts[r] = counts.get(r, 0) + 1
    return out, counts
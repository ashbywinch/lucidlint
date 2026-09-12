# Intentionally-shaped input for the loop-sequence fixer — a shared-accumulator
# chain the rewrite concatenates into chained comprehensions.


def all_text(people):
    chunks = []
    for p in people:
        chunks += [p["name"]]
    for q in people:
        chunks += [q["bio"]]
    return chunks
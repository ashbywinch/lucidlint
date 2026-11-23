def gather(items):
    out = []
    for item in items:
        text = str(item).strip()
        if not text:
            continue
        out.append(text)
    return out

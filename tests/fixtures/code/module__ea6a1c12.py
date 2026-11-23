def collect(rows):
    out = []
    for row in rows:
        text = str(row).strip()
        if not text:
            continue
        out.append(text)
    return out


def never_called() -> int:
    return 42

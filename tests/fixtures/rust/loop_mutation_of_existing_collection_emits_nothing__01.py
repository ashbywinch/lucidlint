def apply(settings_payload, rows):
    result = settings_payload.get("result") or {}
    for row in rows:
        result[row["key"]] = row["value"]
    return result


def fill(payload, xs):
    for x in xs:
        payload["k"] = x
    return payload


results = {}


def update(xs):
    for x in xs:
        results[x] = x
    return results
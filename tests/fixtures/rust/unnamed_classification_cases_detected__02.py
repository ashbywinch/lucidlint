def kind_of(value):
    return value.split("-")[0]


def by_match(raw):
    match raw:
        case {"a": _}:
            return "alpha"
        case {"b": _}:
            return "beta"
        case {"c": _}:
            return "gamma"
    return "unknown"

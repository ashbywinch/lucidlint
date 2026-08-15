def from_line(line: dict[str, Any]) -> Label:
    return Label(line["call_id"])

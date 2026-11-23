def a(items):
    return [item for item in items if item.total > 0 and item.kind != "draft" and item.date is not None]

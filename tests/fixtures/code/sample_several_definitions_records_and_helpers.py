class BatchProjection:
    def get(self, key):
        return self.rows[key]


class BatchRow:
    def __init__(self, values):
        self.values = values


def read_row(store, key):
    return store.get(key)


def write_row(store, key, value):
    store.put(key, value)

def _read_row(store, key):
    return store.get(key)


def _write_row(store, key, value):
    store.put(key, value)


def _drop_row(store, key):
    store.pop(key)

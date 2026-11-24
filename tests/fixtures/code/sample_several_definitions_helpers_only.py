def read_row(store, key):
    return store.get(key)


def write_row(store, key, value):
    store.put(key, value)


def drop_row(store, key):
    store.pop(key)

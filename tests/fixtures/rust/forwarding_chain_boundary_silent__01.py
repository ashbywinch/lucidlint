def get_stored(store, key):
    return store.load(key)


class Server:
    def handle(self, store, key):
        return get_stored(store, key)

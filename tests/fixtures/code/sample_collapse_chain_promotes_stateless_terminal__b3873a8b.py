def fetch_key(key):
    return key

class Store:
    def load(self, key):
        return fetch_key(key)

def get_stored(store, key):
    return store.load(key)

class Facade:
    def fetch(self, store, key):
        return get_stored(store, key)

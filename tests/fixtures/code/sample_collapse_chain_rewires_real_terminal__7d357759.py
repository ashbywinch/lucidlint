class Store:
    def __init__(self):
        self.cache = {}
    def load(self, key):
        return self.cache[key]

def get_stored(store, key):
    return store.load(key)

class Facade:
    def fetch(self, store, key):
        return get_stored(store, key)

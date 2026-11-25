class Registry:
    def get(self, key):
        return key


def __getattr__(name):
    return name


def __dir__():
    return []
